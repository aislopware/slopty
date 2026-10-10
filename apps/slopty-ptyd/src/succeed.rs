//! `slopty-ptyd --succeed`: ask the ptyd running on a socket to become this build, handing it
//! every session ([`slopty_pty::protocol::PtydRequest::Succeed`]), and wait until it has.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, Result};
use slopty_pty::{PtyError, PtydClient};

use crate::daemon::{HANDING, Said, custody_file};
use crate::{CUSTODY, SUCCESSION};

/// How long the running ptyd is waited for to come back as this build once it took the
/// handover: it writes every session's state out before it runs it, which big rings on a slow
/// disk make slow. Past it the handover still stands, since the `exec` happened.
const CAME_BACK: Duration = Duration::from_mins(10);

/// How long a ptyd whose answer was lost (the link failed rather than closed) has to show it
/// went ahead, by marking its custody as handing or saying this build's.
const WENT_AHEAD: Duration = Duration::from_secs(5);

/// How often its custody is read while it does.
const LOOK: Duration = Duration::from_millis(50);

/// Hand the ptyd on `socket` over to this build. Done at once when it is this build's custody
/// already.
pub async fn run(socket: &Path) -> Result<()> {
    let file = custody_file(socket);
    let said = Said::read(&file)
        .with_context(|| format!("{} says no custody and succession", file.display()))?;
    if said.custody == CUSTODY {
        tracing::info!(pid = said.pid, "slopty-ptyd keeps this build's custody already");
        return Ok(());
    }
    anyhow::ensure!(
        said.succession == SUCCESSION,
        "slopty-ptyd (pid {}) hands sessions on another way (succession {}, this build's {})",
        said.pid,
        said.succession,
        SUCCESSION
    );
    let program = std::env::current_exe().context("this build's path")?;
    let (mut ptyd, _exits) = PtydClient::connect(socket).await?;
    // A refusal is the one answer that says nothing happened. A link that closed with no answer
    // is the `exec`; one that failed otherwise may have been either, which the custody file
    // tells: a ptyd that went ahead marks it as handing, then says this build's.
    let accepted = match ptyd.succeed(program).await {
        Ok(()) => true,
        Err(PtyError::Daemon(e)) => anyhow::bail!("slopty-ptyd refused the handover: {e}"),
        Err(e) => {
            tracing::warn!(error = %e, "the handover's answer was lost; reading what ptyd says");
            false
        }
    };
    let start = tokio::time::Instant::now();
    loop {
        let now = Said::read(&file).filter(|now| now.pid == said.pid);
        if now.as_ref().is_some_and(|now| now.custody == CUSTODY) {
            tracing::info!(pid = said.pid, "slopty-ptyd runs this build, every session kept");
            return Ok(());
        }
        let went = accepted || now.as_ref().is_some_and(|now| now.custody == HANDING);
        if !went && start.elapsed() >= WENT_AHEAD {
            anyhow::bail!("slopty-ptyd (pid {}) did not take the handover", said.pid);
        }
        if start.elapsed() >= CAME_BACK {
            // The `exec` happened: whatever the new build does now, there is no going back.
            tracing::warn!(
                pid = said.pid,
                "slopty-ptyd took the handover but has not said this build's custody yet"
            );
            return Ok(());
        }
        tokio::time::sleep(LOOK).await;
    }
}
