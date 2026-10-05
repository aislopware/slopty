//! Whether a newer Slopty is out: the repository's latest release, read at launch and every
//! [`CHECK_EVERY`] after, said quietly in the status bar until this build is the latest.
//!
//! Only the Mac asks: an iPhone or iPad takes its builds from the App Store, not from a release
//! page. Nothing is downloaded or installed here; the bar's line opens the release page.
//! The feed is the repository Cargo names (`CARGO_PKG_REPOSITORY`), so a fork reads its own.

use std::time::Duration;

use gpui::{App, Entity};
use slopty_client::update::{Release, latest_release_feed, newer_release};

use crate::Workspace;

/// How often the feed is read again while the app runs: releases come days apart.
const CHECK_EVERY: Duration = Duration::from_hours(24);

/// Ask the feed now and every [`CHECK_EVERY`], telling `workspace`'s bar what it found. A feed
/// that does not answer leaves the bar as it was.
pub(crate) fn watch(workspace: &Entity<Workspace>, cx: &App) {
    let Some(feed) = latest_release_feed(env!("CARGO_PKG_REPOSITORY")) else { return };
    let workspace = workspace.downgrade();
    cx.spawn(async move |cx| {
        loop {
            match latest(&feed).await {
                Ok(found) => {
                    let told = workspace.update(cx, |ws, cx| {
                        ws.view.update(cx, |v, cx| v.set_release(found, cx));
                    });
                    if told.is_err() {
                        return;
                    }
                }
                Err(why) => tracing::debug!(%why, "the release feed did not answer"),
            }
            cx.background_executor().timer(CHECK_EVERY).await;
        }
    })
    .detach();
}

/// The release newer than this build at `feed`, or `None` when this build is the latest.
async fn latest(feed: &str) -> Result<Option<Release>, slopty_platform::fetch::FetchError> {
    match slopty_platform::fetch::get(feed).await {
        Ok(body) => Ok(newer_release(&body, env!("CARGO_PKG_VERSION"))),
        // No release published yet.
        Err(slopty_platform::fetch::FetchError::Status(404)) => Ok(None),
        Err(e) => Err(e),
    }
}
