//! Codex's own lifecycle command for its app-server daemon, `codex app-server daemon start`,
//! and what it answers.
//!
//! Codex publishes the command for remote clients of a machine reached over SSH. It is
//! idempotent and returns once the app-server answers on its control socket. On success it
//! writes exactly one JSON object to stdout, whose `socketPath` is where the daemon listens. On
//! failure it exits nonzero with its reason on stderr (`Error: …`), which is quoted to the
//! person as Codex wrote it.

use std::path::PathBuf;

use serde::Deserialize;

/// The arguments of `codex` that start its daemon.
pub const START: [&str; 3] = ["app-server", "daemon", "start"];

/// What a start came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Daemon {
    /// It runs, started now or already, listening on `socket`.
    Started {
        /// Its control socket.
        socket: PathBuf,
    },
    /// It did not start, in Codex's words.
    Failed {
        /// Why, as Codex said it.
        message: String,
    },
}

/// The one object a lifecycle command writes on success; only what a start needs of it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Lifecycle {
    status: String,
    socket_path: PathBuf,
}

/// What a start that exited with success `ok`, having written `stdout` and `stderr`, came to.
#[must_use]
pub fn read(ok: bool, stdout: &str, stderr: &str) -> Daemon {
    if !ok {
        let said = stderr.lines().map(str::trim).rfind(|line| !line.is_empty());
        let message = said.map_or("it exited without saying why", |line| {
            line.strip_prefix("Error:").map_or(line, str::trim)
        });
        return Daemon::Failed { message: message.to_owned() };
    }
    let lifecycle =
        stdout.lines().rev().find_map(|line| serde_json::from_str::<Lifecycle>(line.trim()).ok());
    match lifecycle {
        Some(l) if matches!(l.status.as_str(), "started" | "alreadyRunning" | "running") => {
            Daemon::Started { socket: l.socket_path }
        }
        Some(l) => Daemon::Failed { message: format!("it says the app-server is {}", l.status) },
        None => Daemon::Failed { message: "its answer said nothing Slopty reads".to_owned() },
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{Daemon, read};

    /// A start reads as the socket Codex says its daemon listens on, whether it started it now
    /// or found it running; a failure reads as Codex's own last words, without its `Error:`
    /// label; an answer of another shape is no start.
    #[test]
    fn a_start_reads_as_its_socket_or_codexs_words() {
        let started = serde_json::json!({
            "status": "started", "backend": "pid", "pid": 4242,
            "managedCodexPath": "/u/.codex/packages/app-server-daemon/current/bin/codex",
            "managedCodexVersion": "0.157.0",
            "socketPath": "/u/.codex/app-server-control/app-server-control.sock",
            "cliVersion": "0.157.0", "appServerVersion": "0.157.0"
        })
        .to_string();
        let started = started.as_str();
        let socket = PathBuf::from("/u/.codex/app-server-control/app-server-control.sock");
        assert_eq!(read(true, started, ""), Daemon::Started { socket: socket.clone() });
        let running = started.replace("\"started\"", "\"alreadyRunning\"");
        assert_eq!(read(true, &running, "warning: something\n"), Daemon::Started { socket });

        let refused = concat!(
            "warning: stale\n",
            "Error: app server is running but is not managed by codex app-server daemon\n"
        );
        assert_eq!(
            read(false, "", refused),
            Daemon::Failed {
                message: "app server is running but is not managed by codex app-server daemon"
                    .to_owned()
            }
        );
        assert!(matches!(read(false, "", ""), Daemon::Failed { .. }));
        assert!(matches!(read(true, "Started.", ""), Daemon::Failed { .. }));
        let stopped = started.replace("\"started\"", "\"stopped\"");
        assert!(matches!(read(true, &stopped, ""), Daemon::Failed { .. }));
    }
}
