//! `slopty ui`: an agent reads and drives the person's Slopty window from its shell, as the
//! `ui_*` tools do from `slopty mcp` (`slopty_tools::ui`).
//!
//! The ask goes over this machine's worker's control socket (`CtlRequest::Ui`) with the session
//! it runs in (`SLOPTY_SESSION`); the worker hands it to the window of the person in front of
//! that session, and the window's answer comes back. Every verb prints JSON on stdout: what it
//! did and the window after it, or `{"error": {"code", "message"}}` with exit status 1.

use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Result;
use clap::Subcommand;
use slopty_proto::ctl::{CtlReply, CtlRequest, UiCall};
use slopty_proto::handoff::{UiAct, UiChange, UiDone, UiError, UiFailure};

use crate::workerctl;

/// How long the window is waited for: the worker asks each window in turn, and a terminal
/// answers once its shell is up and its command typed.
const UI_WAIT: Duration = Duration::from_secs(60);

/// `slopty ui --help`.
pub const ABOUT: &str = "\
Read and drive the person's Slopty window, from an agent's shell: open a shell, a file, a page \
or the changes in a pane beside the session, show or hide a region, put a chat on screen, lay \
the session's panes at even shares, and say what the window shows.";

/// `slopty ui --help`'s epilogue.
pub const AFTER: &str = "\
The ask goes to the window of the person in front of this shell's session, through this \
machine's worker; a window on someone else's machines is never reached. It never takes the \
keyboard: what would move the chat or the keyboard away from a draft being typed, or hide the \
region the keyboard is in, is refused, and each change is marked in the window with a notice \
naming you. Paths are this machine's, so a file or a folder opens only beside a chat running here.

Every verb prints JSON: `did` (what changed), `made` (a pane it opened, a shell's session id in \
`made.terminal`), `ran` (whether a shell's command ran or waits at the prompt), and `window`: its \
`page`, the `chat` on screen, its `regions` (`sidebar`, `files`, `changes`, `inspector`, \
`terminal`, each `shown`, and `expanded` when its pane is zoomed over the session), its `tabs` \
(the panes beside the session) and whether the person is `typing`. A failure prints \
{\"error\": {\"code\", \"message\"}} and exits 1; `code` is no_window (no window is open), \
unknown (a name the window does not know; its message lists the ones it does), busy (the person \
is typing or in Settings), elsewhere (the chat on screen runs on another machine, or none is on \
screen) or failed.

The same verbs are the ui_* tools of `slopty mcp`.";

/// `slopty ui …`.
#[derive(Subcommand, Debug)]
pub enum UiCmd {
    /// Say what the window shows: its page, the chat on screen, its regions and panes, and
    /// whether the person is typing (the default).
    State,
    /// Open a shell in a pane beside the chat on screen.
    Terminal {
        /// The folder it starts in (default: this one).
        #[arg(long)]
        folder: Option<String>,
        /// A command line to type once its shell is up; one that acts as root, destroys or
        /// spans lines waits at the prompt for the person to run.
        #[arg(long, value_name = "LINE")]
        command: Option<String>,
    },
    /// Show a file (`path`, `path:line`) or a web page (`https://…`) in a pane beside the
    /// session.
    Open {
        /// The file or the address.
        target: String,
        /// The file's line to start at, from 1.
        #[arg(long)]
        line: Option<u32>,
    },
    /// Show the chat's uncommitted changes in a pane beside the session.
    Diff,
    /// Show a region: sidebar, files, changes, inspector or terminal (a new shell beside the
    /// session).
    Show {
        /// The region.
        region: String,
    },
    /// Hide a region: sidebar, files, changes or inspector; the person closes their own shells.
    Hide {
        /// The region.
        region: String,
    },
    /// Show a region that is hidden, hide one that shows.
    Toggle {
        /// The region.
        region: String,
    },
    /// Expand a region: shown, as `show` does.
    Expand {
        /// The region.
        region: String,
    },
    /// Collapse a region: hidden, as `hide` does.
    Collapse {
        /// The region.
        region: String,
    },
    /// Put a chat on screen, by its id, an id prefix or its title (default: your own).
    Chat {
        /// The chat.
        chat: Option<String>,
    },
    /// Lay the session's panes at even shares, showing a file or a page beside it first when
    /// given.
    Split {
        /// The file or the address.
        target: Option<String>,
        /// The file's line to start at, from 1.
        #[arg(long)]
        line: Option<u32>,
    },
}

/// The act `cmd` asks for, paths read against `cwd`.
fn act(cmd: UiCmd, cwd: &Path) -> Result<UiAct, UiFailure> {
    use slopty_tools::ui::{folder, target};
    let region = |region: String, change| Ok(UiAct::Region { region, change });
    match cmd {
        UiCmd::State => Ok(UiAct::State),
        UiCmd::Terminal { folder: given, command } => Ok(UiAct::Terminal {
            folder: Some(folder(given.as_deref(), cwd)?),
            command,
            region: None,
        }),
        UiCmd::Open { target: given, line } => Ok(UiAct::Open(target(&given, line, cwd)?)),
        UiCmd::Diff => Ok(UiAct::Open(slopty_proto::handoff::UiTarget::Diff)),
        UiCmd::Show { region: r } => region(r, UiChange::Show),
        UiCmd::Hide { region: r } => region(r, UiChange::Hide),
        UiCmd::Toggle { region: r } => region(r, UiChange::Toggle),
        UiCmd::Expand { region: r } => region(r, UiChange::Expand),
        UiCmd::Collapse { region: r } => region(r, UiChange::Collapse),
        UiCmd::Chat { chat } => Ok(UiAct::Chat { chat }),
        UiCmd::Split { target: given, line } => {
            Ok(UiAct::Split(given.map(|given| target(&given, line, cwd)).transpose()?))
        }
    }
}

/// `slopty ui`: `cmd` asked of the window, its answer printed as JSON.
pub async fn run(cmd: Option<UiCmd>, data_dir: &Path) -> Result<ExitCode> {
    let cwd = std::env::current_dir()?;
    let outcome = match act(cmd.unwrap_or(UiCmd::State), &cwd) {
        Ok(act) => ask(data_dir, act).await,
        Err(failure) => Err(failure),
    };
    println!("{}", serde_json::to_string_pretty(&slopty_tools::ui::json(&outcome))?);
    Ok(match outcome {
        Ok(_) => ExitCode::SUCCESS,
        Err(failure) => {
            eprintln!("slopty: {failure}");
            ExitCode::FAILURE
        }
    })
}

/// Ask `act` of the person's window through this machine's worker, as the session this runs in.
pub async fn ask(data_dir: &Path, act: UiAct) -> Result<UiDone, UiFailure> {
    ask_at(&workerctl::socket(data_dir), act).await
}

/// [`ask`] through the worker's control socket at `socket`.
async fn ask_at(socket: &Path, act: UiAct) -> Result<UiDone, UiFailure> {
    let session =
        crate::handoff::session().map_err(|e| UiFailure::new(UiError::Failed, e.to_string()))?;
    let request = CtlRequest::Ui(UiCall { session, act });
    let reply = tokio::time::timeout(UI_WAIT, crate::handoff::exchange(socket, &request)).await;
    match reply {
        Ok(Ok(CtlReply::UiDone(done))) => Ok(*done),
        Ok(Ok(CtlReply::UiFailed(failure))) => Err(failure),
        Ok(Ok(CtlReply::Error { message })) => Err(UiFailure::new(UiError::Failed, message)),
        Ok(Ok(other)) => {
            Err(UiFailure::new(UiError::Failed, format!("the worker answered {other:?}")))
        }
        Ok(Err(e)) => Err(UiFailure::new(
            UiError::NoWindow,
            format!("no Slopty worker answers on this machine ({e}), so no window can be reached"),
        )),
        Err(_elapsed) => {
            Err(UiFailure::new(UiError::NoWindow, "the window did not answer in time"))
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::*;

    #[derive(clap::Parser, Debug)]
    struct Line {
        #[command(subcommand)]
        cmd: UiCmd,
    }

    fn parsed(args: &[&str], cwd: &Path) -> Result<UiAct, UiFailure> {
        let line = Line::try_parse_from(std::iter::once("ui").chain(args.iter().copied()))
            .map_err(|e| UiFailure::new(UiError::Failed, e.to_string()))?;
        act(line.cmd, cwd)
    }

    /// Each verb becomes the act it names: a terminal in this folder or the one named, a file
    /// with its line, a page, a region's change, a chat, a split; a file or a folder that is not
    /// there is unknown before the window is asked.
    #[test]
    fn each_verb_becomes_its_act() -> std::io::Result<()> {
        use slopty_proto::handoff::UiTarget;
        let dir = tempfile::tempdir()?;
        let cwd = &dir.path().canonicalize()?;
        std::fs::write(cwd.join("notes.md"), "# Notes\n")?;
        let here = cwd.to_string_lossy().into_owned();
        let notes = cwd.join("notes.md").to_string_lossy().into_owned();
        assert_eq!(parsed(&["state"], cwd), Ok(UiAct::State));
        assert_eq!(
            parsed(&["terminal", "--command", "cargo test"], cwd),
            Ok(UiAct::Terminal {
                folder: Some(here),
                command: Some("cargo test".to_owned()),
                region: None,
            })
        );
        let file = UiTarget::File { path: notes, line: Some(4) };
        assert_eq!(parsed(&["open", "notes.md:4"], cwd), Ok(UiAct::Open(file.clone())));
        assert_eq!(
            parsed(&["open", "notes.md", "--line", "4"], cwd),
            Ok(UiAct::Open(file.clone()))
        );
        assert_eq!(
            parsed(&["open", "https://example.com"], cwd),
            Ok(UiAct::Open(UiTarget::Page { url: "https://example.com/".to_owned() }))
        );
        assert_eq!(parsed(&["diff"], cwd), Ok(UiAct::Open(UiTarget::Diff)));
        let expand = UiAct::Region { region: "inspector".to_owned(), change: UiChange::Expand };
        assert_eq!(parsed(&["expand", "inspector"], cwd), Ok(expand));
        let hide = UiAct::Region { region: "sidebar".to_owned(), change: UiChange::Hide };
        assert_eq!(parsed(&["hide", "sidebar"], cwd), Ok(hide));
        assert_eq!(
            parsed(&["chat", "Fix the build"], cwd),
            Ok(UiAct::Chat { chat: Some("Fix the build".to_owned()) })
        );
        assert_eq!(parsed(&["split", "notes.md:4"], cwd), Ok(UiAct::Split(Some(file))));
        let code = |args: &[&str]| parsed(args, cwd).map_err(|f| f.code);
        assert_eq!(code(&["open", "gone.md"]), Err(UiError::Unknown));
        assert_eq!(code(&["terminal", "--folder", "nowhere"]), Err(UiError::Unknown));
        Ok(())
    }

    /// With no worker on its socket, an ask says no window can be reached.
    #[tokio::test]
    async fn with_no_worker_no_window_is_reached() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("worker.sock");
        let failure = ask_at(&socket, UiAct::State).await.unwrap_err();
        assert_eq!(failure.code, UiError::NoWindow, "{failure}");
    }
}
