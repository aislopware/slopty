//! The Slopty iOS app.
//!
//! A static library the app bundle links whole: it defines `main`, which hands the process to
//! `UIApplicationMain`, and the two classes UIKit drives, the app delegate and the window
//! scene's delegate. The app delegate installs the notification delegate while the app
//! finishes launching; the scene delegate starts the tokio runtime and GPUI, embedded in
//! UIKit's run loop, once the window scene connects, and forwards the scene's lifecycle to
//! `gpui_ios`. Everything the app does lives in `slopty-app`, shared with the macOS app.

#![cfg(target_os = "ios")]

use std::ffi::{c_char, c_int, c_void};

use gpui::WindowOptions;
use gpui_ios::ios::ffi;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObjectProtocol};
use objc2::{ClassType as _, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_foundation::{NSDictionary, NSObject, NSSet, NSString};
use objc2_ui_kit::{
    UIApplication, UIApplicationDelegate, UIApplicationLaunchOptionsKey, UIOpenURLContext,
    UIResponder, UIScene, UISceneConnectionOptions, UISceneDelegate, UISceneSession, UIWindowScene,
    UIWindowSceneDelegate,
};

/// The process entry point: the bundle has no other code, so the linker takes this `main`.
///
/// `Info.plist` names [`SceneDelegate`] as the scene's delegate class, and UIKit looks both
/// classes up by name, so they are registered with the runtime before it starts.
#[unsafe(no_mangle)]
pub extern "C" fn main(_argc: c_int, _argv: *const *const c_char) -> c_int {
    slopty_crash::install(slopty_crash::Process::IosApp, &slopty_platform::dirs::data_dir());
    let Some(mtm) = MainThreadMarker::new() else {
        return 1;
    };
    let _registered = (AppDelegate::class(), SceneDelegate::class());
    UIApplication::main(None, Some(&NSString::from_str(AppDelegate::NAME)), mtm)
}

define_class!(
    // SAFETY:
    // - `UIResponder` has no subclassing requirements beyond being used on the main thread.
    // - `AppDelegate` does not implement `Drop`.
    #[unsafe(super(UIResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SloptyAppDelegate"]
    struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl UIApplicationDelegate for AppDelegate {
        /// Runs before any scene connects: the notification delegate has to be in place by
        /// the time this returns, or the tap that launched the app is never delivered
        /// (`slopty_platform::notify::install`). GPUI starts only once the scene connects.
        #[unsafe(method(application:didFinishLaunchingWithOptions:))]
        fn did_finish_launching(
            &self,
            _application: &UIApplication,
            _options: Option<&NSDictionary<UIApplicationLaunchOptionsKey, AnyObject>>,
        ) -> bool {
            init_logging();
            slopty_platform::notify::install();
            true
        }

        #[unsafe(method(applicationDidReceiveMemoryWarning:))]
        fn did_receive_memory_warning(&self, application: &UIApplication) {
            ffi::gpui_ios_did_receive_memory_warning(object_ptr(application));
        }

        #[unsafe(method(applicationWillTerminate:))]
        fn will_terminate(&self, application: &UIApplication) {
            ffi::gpui_ios_will_terminate(object_ptr(application));
        }
    }
);

define_class!(
    // SAFETY:
    // - `UIResponder` has no subclassing requirements beyond being used on the main thread.
    // - `SceneDelegate` does not implement `Drop`.
    #[unsafe(super(UIResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "SloptySceneDelegate"]
    struct SceneDelegate;

    unsafe impl NSObjectProtocol for SceneDelegate {}

    unsafe impl UISceneDelegate for SceneDelegate {
        #[unsafe(method(scene:willConnectToSession:options:))]
        fn will_connect(
            &self,
            scene: &UIScene,
            _session: &UISceneSession,
            options: &UISceneConnectionOptions,
        ) {
            let Some(scene) = scene.downcast_ref::<UIWindowScene>() else {
                return;
            };
            ffi::gpui_ios_set_window_scene(object_ptr(scene));
            // Each window drives its own display link, paused while idle (gpui_ios frame pacing).
            run();
            // A link that launched the app comes with the connection, not as an open later.
            // SAFETY: UIKit rule: `-[UISceneConnectionOptions URLContexts]` takes no argument
            // and returns an `NSSet<UIOpenURLContext *>`. The header marks it nonnull, but a
            // launch with no link returns nil, which the generated binding panics on, so the
            // result is read as optional.
            let contexts: Option<Retained<NSSet<UIOpenURLContext>>> =
                unsafe { msg_send![options, URLContexts] };
            if let Some(contexts) = contexts {
                open_links(&contexts);
            }
        }

        #[unsafe(method(sceneWillEnterForeground:))]
        fn will_enter_foreground(&self, scene: &UIScene) {
            ffi::gpui_ios_will_enter_foreground(object_ptr(scene));
        }

        #[unsafe(method(sceneDidBecomeActive:))]
        fn did_become_active(&self, scene: &UIScene) {
            ffi::gpui_ios_did_become_active(object_ptr(scene));
        }

        #[unsafe(method(sceneWillResignActive:))]
        fn will_resign_active(&self, scene: &UIScene) {
            ffi::gpui_ios_will_resign_active(object_ptr(scene));
        }

        #[unsafe(method(sceneDidEnterBackground:))]
        fn did_enter_background(&self, scene: &UIScene) {
            ffi::gpui_ios_did_enter_background(object_ptr(scene));
        }

        #[unsafe(method(scene:openURLContexts:))]
        fn open_url_contexts(&self, _scene: &UIScene, contexts: &NSSet<UIOpenURLContext>) {
            open_links(contexts);
        }
    }

    unsafe impl UIWindowSceneDelegate for SceneDelegate {}
);

/// Hand each link the system opened the app with to the workspace (`slopty_app::open_link`),
/// which takes only its own kind. Straight there rather than through GPUI's open-URL callback,
/// which `gpui_ios` lets no embedder register, and which a cold launch would reach before
/// the workspace listens.
fn open_links(contexts: &NSSet<UIOpenURLContext>) {
    for context in contexts {
        if let Some(url) = context.URL().absoluteString() {
            slopty_app::open_link(url.to_string());
        }
    }
}

/// An Objective-C object as the untyped pointer `gpui_ios`'s entry points take. They borrow it
/// for the call; UIKit keeps each object alive for longer than that.
const fn object_ptr<T: objc2::Message>(object: &T) -> *mut c_void {
    std::ptr::from_ref(object).cast_mut().cast()
}

/// Starts tokio and GPUI once the window scene is connected. A failure to start is logged.
fn run() {
    slopty_platform::playback_audio_session();
    // The link's writer, the session pumps and noq's drivers carry every keystroke and echo.
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .on_thread_start(slopty_platform::user_interactive_thread)
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            tracing::error!(error = %e, "tokio runtime");
            return;
        }
    };
    // The runtime lives as long as the process; UIKit never returns from `UIApplicationMain`.
    let handle = Box::leak(Box::new(runtime)).handle().clone();
    ffi::set_app_callback(Box::new(move |cx| {
        gpui_kit::init(cx);
        if let Err(e) = slopty_app::open_workspace(cx, handle, |_| WindowOptions::default()) {
            tracing::error!(error = %e, "open workspace");
            return;
        }
        // After `open_workspace`, as on the Mac: the menus read their chords from the keymap
        // the workspace fills in. An iPad with a keyboard shows them in its menu bar and in
        // the sheet a held ⌘ brings up.
        slopty_app::set_app_menus(cx, || slopty_app::menus::menus(Vec::new(), true));
    }));
    ffi::run_app_with_assets(gpui_kit::assets::Assets);
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
