//! The Slopty macOS app: logging, tokio runtime, latency-critical activity, then the shared
//! workspace from `slopty-app`.

#![forbid(unsafe_code)]

use anyhow::Result;

/// Run the app: the crash reporter, logging, the runtime, then the workspace window.
pub fn main() -> Result<()> {
    slopty_proto::wire::set_commit(option_env!("SLOPTY_COMMIT"));
    slopty_crash::install(slopty_crash::Process::App, &slopty_platform::dirs::data_dir());
    let log = tracing_subscriber::fmt().with_env_filter(
        tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
    );
    match log_file() {
        Some(file) => log.with_ansi(false).with_writer(std::sync::Arc::new(file)).init(),
        None => log.with_writer(std::io::stderr).init(),
    }
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
    // A `slopty://` link the system opened the app with goes to the workspace, held until it
    // listens (`slopty_app::open_link`): a server to connect to.
    app.on_open_urls(|urls| urls.into_iter().for_each(slopty_app::open_link));
    app.run(move |cx| {
        gpui_kit::init(cx);
        slopty_app::open_workspace(cx, handle);
    });
    Ok(())
}

/// The size past which the app's log file is begun afresh at launch.
const LOG_CAP: u64 = 32 << 20;

/// Where the log goes when nothing reads the app's standard error, as when it was opened from
/// the Finder or the Dock (`/dev/null`): beside the server's and the worker's
/// (`~/Library/Logs/Slopty/slopty-app.log`), so what the app saw of a link that went quiet can be
/// read afterwards. A terminal, or a pipe or file the app was started with (an e2e run's
/// harness), reads it there instead. Begun afresh once it passes [`LOG_CAP`].
fn log_file() -> Option<std::fs::File> {
    use std::io::IsTerminal as _;
    use std::os::unix::fs::FileTypeExt as _;
    let unread = !std::io::stderr().is_terminal()
        && std::fs::metadata("/dev/fd/2").is_ok_and(|meta| meta.file_type().is_char_device());
    if !unread {
        return None;
    }
    let dir = slopty_platform::dirs::home().join("Library").join("Logs").join("Slopty");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("slopty-app.log");
    let mut open = std::fs::OpenOptions::new();
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() > LOG_CAP) {
        open.write(true).create(true).truncate(true);
    } else {
        open.append(true).create(true);
    }
    open.open(path).ok()
}
