//! `slopty browse` and `slopty edit`: a shell's web pages and editor, on the client in front of
//! it.
//!
//! Every Slopty session has `BROWSER` and `EDITOR` naming this binary under other names (by
//! absolute path), and an `open` (`xdg-open` on Linux) first on its `PATH`
//! (`slopty_pty::shell_integration`); [`by_name`] tells them apart by `argv[0]`:
//!
//! * `open`/`xdg-open` with nothing but web addresses asks the worker to open them on the client,
//!   and with one existing file shows it in a tile there (`open report.pdf`); anything else (an
//!   application, a flag, a folder, a saved page) goes to the system's own, untouched.
//! * `slopty-browser <url>` (`BROWSER`) is `slopty browse <url>`.
//! * `slopty-editor [+line] <file>` (`EDITOR`) is `slopty edit --wait`: the file shows in a tile
//!   beside the shell, and the command returns once the person is done with it, 0, or 1 when they
//!   gave the edit up.
//!
//! When no worker answers, or no client takes it, the page goes to the system's opener and the
//! file to `vi` in this terminal, as they would without Slopty, and stderr says why. A page a
//! client offered rather than opened is shown there as a notice (`docs/decisions/terminal.md`,
//! "A shell's browser and editor are the client's").

use std::ffi::OsString;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

use anyhow::{Result, bail};
use slopty_proto::ctl::{CtlReply, CtlRequest, EditAsk, Handed};
use slopty_proto::handoff::{EditOutcome, is_openable, page};
use slopty_pty::shell_integration::{BIN, BROWSER_SHIM, CLIPBOARD_COMMANDS, EDITOR_SHIM, OPENER};

use crate::{hook, workerctl};

/// How long a page may take to be handed over: the worker asks each client in turn, a few
/// seconds each.
const OPEN_WAIT: Duration = Duration::from_secs(20);

/// The editor used where no client can show the file: the one `git` falls back to.
const FALLBACK_EDITOR: &str = "vi";

/// Run as the command `argv[0]` names, when it is one of the handoff commands or a clipboard
/// command (`crate::clipboard`); `None` for `slopty` itself.
pub async fn by_name() -> Option<Result<ExitCode>> {
    let mut args = std::env::args_os();
    let arg0 = PathBuf::from(args.next()?);
    let name = arg0.file_name()?.to_str()?.to_owned();
    let rest: Vec<OsString> = args.collect();
    let data_dir = slopty_platform::dirs::data_dir();
    Some(match name.as_str() {
        n if n == OPENER => open_shim(&data_dir, rest).await,
        BROWSER_SHIM => browse(&data_dir, &lossy(&rest)).await,
        EDITOR_SHIM => edit(&data_dir, true, rest).await,
        n if CLIPBOARD_COMMANDS.contains(&n) => crate::clipboard::run(n, &data_dir, rest).await,
        _ => return None,
    })
}

fn lossy(args: &[OsString]) -> Vec<String> {
    args.iter().map(|a| a.to_string_lossy().into_owned()).collect()
}

/// `open`: web addresses go to the client, and so does one file, shown in a tile beside the
/// shell (a picture, a PDF, a text); any other use is the system's.
async fn open_shim(data_dir: &Path, args: Vec<OsString>) -> Result<ExitCode> {
    if let Some(file) = tile_file(&args) {
        match edit(data_dir, false, vec![file.to_owned()]).await {
            Ok(code) => return Ok(code),
            Err(e) => eprintln!("slopty: {e}; opening {} here", Path::new(file).display()),
        }
        return Err(system_opener(&args));
    }
    let urls: Option<Vec<String>> =
        args.iter().map(|a| a.to_str().filter(|u| is_openable(u)).map(str::to_owned)).collect();
    let here = match urls {
        Some(urls) if !urls.is_empty() => hand_over(data_dir, &urls).await,
        _ => return Err(system_opener(&args)),
    };
    if here.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }
    Err(system_opener(&here.iter().map(OsString::from).collect::<Vec<_>>()))
}

/// `slopty browse`: each web address to the client, anything else (a local file, as `cargo doc
/// --open` gives) to the system's opener, with the addresses no client took.
pub async fn browse(data_dir: &Path, targets: &[String]) -> Result<ExitCode> {
    if targets.is_empty() {
        bail!("nothing to open");
    }
    let (urls, rest): (Vec<String>, Vec<String>) =
        targets.iter().cloned().partition(|t| is_openable(t));
    let mut here = if urls.is_empty() { Vec::new() } else { hand_over(data_dir, &urls).await };
    here.extend(rest);
    if here.is_empty() {
        return Ok(ExitCode::SUCCESS);
    }
    Err(system_opener(&here.iter().map(OsString::from).collect::<Vec<_>>()))
}

/// Ask the worker to open each of `urls` on a client, saying on stderr what became of each
/// that did not simply open. The ones to open on this machine instead: those no client took,
/// and, once no worker answers, that one and every one after it. A page a client took is never
/// opened here too.
async fn hand_over(data_dir: &Path, urls: &[String]) -> Vec<String> {
    let socket = workerctl::socket(data_dir);
    let session = hook::session().ok().flatten();
    let mut here = Vec::new();
    for (nth, url) in urls.iter().enumerate() {
        let request = CtlRequest::Open { session, url: url.clone() };
        let host = page(url).map_or_else(|| url.clone(), |p| p.host);
        match tokio::time::timeout(OPEN_WAIT, hook::exchange(&socket, &request)).await {
            Ok(Ok(CtlReply::Handoff(Handed::Taken { .. }))) => {}
            Ok(Ok(CtlReply::Handoff(Handed::Offered { client, why }))) => {
                eprintln!("slopty: {host} is offered on {client}, not opened: {why}");
            }
            Ok(Ok(CtlReply::Handoff(Handed::Nobody { why }))) => {
                eprintln!("slopty: {why}; opening {host} here");
                here.push(url.clone());
            }
            Ok(Ok(CtlReply::Error { message })) => {
                eprintln!("slopty: the worker did not hand {host} over: {message}");
                here.push(url.clone());
            }
            Ok(Ok(other)) => {
                eprintln!("slopty: the worker answered {other:?}; opening {host} here");
                here.push(url.clone());
            }
            Ok(Err(e)) => {
                tracing::debug!(error = %e, "no worker to open on");
                here.extend(urls.iter().skip(nth).cloned());
                break;
            }
            Err(_elapsed) => {
                eprintln!("slopty: the worker did not answer; opening {host} here");
                here.extend(urls.iter().skip(nth).cloned());
                break;
            }
        }
    }
    here
}

/// Replace this process with the system's opener, `args` as given; the error when it could
/// not run.
fn system_opener(args: &[OsString]) -> anyhow::Error {
    let Some(opener) = real_opener() else {
        return anyhow::anyhow!("no {OPENER} here besides Slopty's");
    };
    let e = Command::new(&opener).args(args).exec();
    anyhow::anyhow!("run {}: {e}", opener.display())
}

/// The system's opener: the first `open` (`xdg-open`) on `PATH` that is not Slopty's, as the
/// program would have run without Slopty; macOS's own when there is none.
fn real_opener() -> Option<PathBuf> {
    let ours = std::env::var_os(BIN).map(PathBuf::from);
    let me = std::env::current_exe().ok().and_then(|p| std::fs::canonicalize(p).ok());
    let found = std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .filter(|dir| ours.as_ref() != Some(dir))
            .map(|dir| dir.join(OPENER))
            .find(|candidate| {
                candidate.is_file() && std::fs::canonicalize(candidate).ok().as_ref() != me.as_ref()
            })
    });
    found.or_else(|| cfg!(target_os = "macos").then(|| PathBuf::from("/usr/bin/open")))
}

/// The file `open`'s arguments name for a tile: one existing regular file and no flag. A web
/// page saved as a file (`.html`) is not one: it wants a browser, not its source.
fn tile_file(args: &[OsString]) -> Option<&OsString> {
    let [file] = args else { return None };
    let path = Path::new(file);
    let page = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("html") || e.eq_ignore_ascii_case("htm"));
    let flag = file.to_str().is_some_and(|f| f.starts_with('-'));
    (!flag && !page && path.is_file()).then_some(file)
}

/// What an editor's command line names: `[+line] <file>`, the one shape every program that
/// runs `$EDITOR` gives it.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Target {
    path: PathBuf,
    line: Option<u32>,
}

fn target(args: &[OsString]) -> Option<Target> {
    match args {
        [file] => Some(Target { path: PathBuf::from(file), line: None }),
        [line, file] => {
            let line = line.to_str()?.strip_prefix('+')?.parse().ok()?;
            Some(Target { path: PathBuf::from(file), line: Some(line) })
        }
        _ => None,
    }
}

/// `slopty edit`: show the file in a tile on the client in front of this shell; with `wait`,
/// return once the person is done. Any other shape of arguments, no worker, or no client that
/// can show a file: `vi` here, when waiting, as `$EDITOR` would have been.
pub async fn edit(data_dir: &Path, wait: bool, args: Vec<OsString>) -> Result<ExitCode> {
    let Some(Target { path, line }) = target(&args) else {
        return fallback(wait, &args, "Slopty edits one file");
    };
    let path = std::path::absolute(&path)?;
    let ask = EditAsk {
        session: hook::session().ok().flatten(),
        path: path.to_string_lossy().into_owned(),
        line,
        wait,
    };
    let reply = hook::exchange(&workerctl::socket(data_dir), &CtlRequest::Edit(ask)).await;
    match reply {
        Ok(CtlReply::Handoff(
            Handed::Edited { outcome: EditOutcome::Done } | Handed::Taken { .. },
        )) => Ok(ExitCode::SUCCESS),
        Ok(CtlReply::Handoff(Handed::Edited { outcome: EditOutcome::Cancelled })) => {
            eprintln!("slopty: the edit of {} was given up", path.display());
            Ok(ExitCode::FAILURE)
        }
        Ok(CtlReply::Handoff(Handed::Lost)) => {
            eprintln!("slopty: the client showing {} went away", path.display());
            Ok(ExitCode::FAILURE)
        }
        Ok(CtlReply::Handoff(Handed::Nobody { why })) => fallback(wait, &args, &why.to_string()),
        Ok(CtlReply::Error { message }) => fallback(wait, &args, &message),
        Ok(other) => fallback(wait, &args, &format!("the worker said {other:?}")),
        Err(e) => fallback(wait, &args, &format!("no worker answered ({e})")),
    }
}

/// Where Slopty cannot edit: `vi` in this terminal for a program waiting on its editor, else
/// the reason.
fn fallback(wait: bool, args: &[OsString], why: &str) -> Result<ExitCode> {
    if !wait {
        bail!("{why}");
    }
    eprintln!("slopty: {why}; editing in {FALLBACK_EDITOR} here");
    let e = Command::new(FALLBACK_EDITOR).args(args).exec();
    bail!("run {FALLBACK_EDITOR}: {e}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `open <file>` shows one existing file in a tile; a flag, a page, a folder, a file that
    /// is not there and two files are the system opener's.
    #[test]
    fn open_shows_one_existing_file_in_a_tile() -> Result<(), std::io::Error> {
        let dir = tempfile::tempdir()?;
        let doc = dir.path().join("report.pdf");
        let page = dir.path().join("index.HTML");
        std::fs::write(&doc, b"%PDF-1.7")?;
        std::fs::write(&page, b"<html>")?;
        let arg = |p: &Path| OsString::from(p);
        assert_eq!(tile_file(&[arg(&doc)]), Some(&arg(&doc)));
        for other in [
            vec![arg(&page)],
            vec![arg(dir.path())],
            vec![arg(&dir.path().join("gone.txt"))],
            vec![arg(&doc), arg(&doc)],
            vec![OsString::from("-a"), OsString::from("Safari")],
            Vec::new(),
        ] {
            assert_eq!(tile_file(&other), None, "{other:?}");
        }
        Ok(())
    }

    /// `$EDITOR`'s arguments are a file, or a `+line` and a file; anything else is not ours.
    #[test]
    fn an_editor_command_line_is_a_file_and_maybe_a_line() {
        let args = |words: &[&str]| words.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(
            target(&args(&[".git/COMMIT_EDITMSG"])),
            Some(Target { path: ".git/COMMIT_EDITMSG".into(), line: None })
        );
        assert_eq!(
            target(&args(&["+12", "src/main.rs"])),
            Some(Target { path: "src/main.rs".into(), line: Some(12) })
        );
        for other in [&[][..], &["-n", "f"], &["a", "b", "c"], &["+x", "f"]] {
            assert_eq!(target(&args(other)), None, "{other:?}");
        }
    }
}
