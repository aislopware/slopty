//! The Slopty macOS app: logging, tokio runtime, latency-critical activity, then the shared
//! workspace from `slopty-app`.

#![forbid(unsafe_code)]

use anyhow::Result;

/// Run the app: the crash reporter, logging, the runtime, then the workspace window.
pub fn main() -> Result<()> {
    slopty_proto::wire::set_commit(option_env!("SLOPTY_COMMIT"));
    slopty_crash::install(slopty_crash::Process::App, &slopty_platform::dirs::data_dir());
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    // The link's writer, the session pumps and noq's drivers carry every keystroke and echo.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .on_thread_start(slopty_platform::user_interactive_thread)
        .build()?;
    let handle = runtime.handle().clone();
    // Held for the whole run: App Nap would otherwise throttle the link's heartbeats while the
    // window is covered and the direct path would be abandoned.
    let _activity = slopty_platform::Activity::latency_critical("Slopty remote session");

    // The shell's application: its icons and fonts, its image fetches and its Dock-icon reopen.
    let app = slopty_shell::application(handle.clone());
    app.run(move |cx| {
        gpui_kit::init(cx);
        slopty_app::open_workspace(cx, handle);
    });
    Ok(())
}
