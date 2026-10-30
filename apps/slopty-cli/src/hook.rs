//! `slopty hook`: Claude Code's hooks, relayed to this machine's worker.
//!
//! A Claude Code Slopty starts runs this for each of its hooks (`slopty_agent::hooks`), always
//! asynchronously: Claude Code never waits on it, and nothing it prints reaches Claude Code. It
//! reads the hook's JSON on stdin, notes when it started (two relays may arrive out of order),
//! and hands both to the worker over its control socket as the session it runs in, vouched by
//! that session's token (`CtlRequest::Hook`). The tool's output a `PostToolUse` carries is left
//! out: the worker reads it from the transcript, and it may be large.
//!
//! It always exits 0, quietly: outside a Slopty session, or with no worker to hear it, a hook is
//! simply not heard.

use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

use slopty_proto::ctl::{CtlReply, CtlRequest, SESSION_TOKEN_ENV};
use tokio::io::{AsyncRead, AsyncReadExt as _};

use crate::workerctl;

/// The longest hook payload read; one longer is not relayed.
const MAX_PAYLOAD: u64 = 8 << 20;

/// How long the worker may take to hear it: far less than the hook's own timeout.
const RELAY_WAIT: Duration = Duration::from_secs(3);

/// The field of a hook that carries a tool's output.
const TOOL_RESPONSE: &str = "tool_response";

/// `slopty hook`.
#[expect(
    clippy::disallowed_methods,
    reason = "a hook is stamped on the wall clock in microseconds, finer than `WallMs`"
)]
pub async fn run(data_dir: &Path) -> ExitCode {
    let at_us = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX));
    let Some(payload) = payload(tokio::io::stdin()).await else {
        tracing::debug!("a hook's payload could not be read whole");
        return ExitCode::SUCCESS;
    };
    let (Ok(Some(session)), Ok(token)) =
        (crate::handoff::session(), std::env::var(SESSION_TOKEN_ENV))
    else {
        return ExitCode::SUCCESS;
    };
    let request = CtlRequest::Hook { session, token, at_us, payload: lean(payload) };
    let socket = workerctl::socket(data_dir);
    match tokio::time::timeout(RELAY_WAIT, crate::handoff::exchange(&socket, &request)).await {
        Ok(Ok(CtlReply::Ok { .. })) => {}
        Ok(Ok(other)) => tracing::debug!(?other, "the worker did not take the hook"),
        Ok(Err(e)) => tracing::debug!(error = %e, "no worker heard the hook"),
        Err(_elapsed) => tracing::debug!("the worker did not answer the hook in time"),
    }
    ExitCode::SUCCESS
}

/// The hook's JSON, read whole from `input`: `None` when it does not read as text or runs past
/// [`MAX_PAYLOAD`].
async fn payload(input: impl AsyncRead + Unpin) -> Option<String> {
    let mut payload = String::new();
    let mut input = input.take(MAX_PAYLOAD.saturating_add(1));
    let read = input.read_to_string(&mut payload).await;
    (read.is_ok() && input.limit() > 0).then_some(payload)
}

/// `payload` without the tool's output; as it is when it is not a JSON object.
fn lean(payload: String) -> String {
    let Ok(mut hook) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&payload)
    else {
        return payload;
    };
    if hook.remove(TOOL_RESPONSE).is_none() {
        return payload;
    }
    serde_json::to_string(&hook).unwrap_or(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A payload is relayed up to [`MAX_PAYLOAD`] and not past it, nor one that is not text.
    #[tokio::test]
    async fn a_payload_is_read_whole_up_to_its_cap() {
        let most = usize::try_from(MAX_PAYLOAD).unwrap();
        let whole = vec![b'a'; most];
        let read = payload(whole.as_slice()).await.expect("all of it");
        assert_eq!(read.len(), most);
        let over = vec![b'a'; most.saturating_add(1)];
        assert_eq!(payload(over.as_slice()).await, None, "one byte too long");
        assert_eq!(payload(&b"{\"a\":\"\xff\"}"[..]).await, None, "not text");
        assert_eq!(payload(&b""[..]).await.as_deref(), Some(""));
    }

    /// A tool's output is left out of what is relayed, everything else kept; a payload that is
    /// not an object goes as it came.
    #[test]
    fn the_tools_output_stays_behind() {
        let relayed = lean(
            r#"{"hook_event_name":"PostToolUse","tool_name":"Read","tool_response":{"content":"…"}}"#
                .to_owned(),
        );
        let hook: serde_json::Value = serde_json::from_str(&relayed).unwrap();
        assert_eq!(hook["tool_name"], "Read");
        assert!(hook.get(TOOL_RESPONSE).is_none());
        assert_eq!(lean("not json".to_owned()), "not json");
        let stop = r#"{"hook_event_name":"Stop"}"#;
        assert_eq!(lean(stop.to_owned()), stop);
    }
}
