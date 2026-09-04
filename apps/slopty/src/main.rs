//! The Slopty macOS app: logging, tokio runtime, latency-critical activity, then the shared
//! workspace from `slopty-app`.

use anyhow::Result;
use gpui::{Bounds, WindowBounds, WindowOptions, px, size};

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(
            |_| {
                // iroh's path events carry the abandon reason; always keep them.
                tracing_subscriber::EnvFilter::new("info,iroh::_events::path=debug")
            },
        ))
        .with_writer(std::io::stderr)
        .init();
    let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    let handle = runtime.handle().clone();
    // Held for the whole run: App Nap would otherwise throttle the link's heartbeats while the
    // window is covered and the direct path would be abandoned.
    let _activity = slopty_platform::Activity::latency_critical("Slopty remote session");

    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        let bounds = Bounds::centered(None, size(px(1280.0), px(800.0)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(gpui::TitlebarOptions {
                title: Some("Slopty".into()),
                appears_transparent: true,
                traffic_light_position: Some(gpui::point(px(12.0), px(12.0))),
            }),
            ..Default::default()
        };
        if let Err(e) = slopty_app::open_workspace(cx, handle, options) {
            tracing::error!(error = %e, "open window");
            cx.quit();
            return;
        }
        cx.activate(true);
    });
    Ok(())
}
