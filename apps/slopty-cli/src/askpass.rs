//! `slopty` as `ssh`'s askpass helper: run by an `ssh` the app signs in with, which names the
//! socket to ask in its environment ([`slopty_deploy::askpass::SOCK`]) and the question as the
//! one argument. The answer goes to standard output, which is the pipe `ssh` reads, and
//! nowhere else; a refusal, or no socket, exits 1, which `ssh` takes as the person saying no.

use std::io::Write as _;
use std::process::ExitCode;

use slopty_deploy::{ExposeSecret as _, askpass};

/// The helper's run when this process is one (`ssh` set [`askpass::SOCK`] and `SSH_ASKPASS`);
/// `None` for any other run of `slopty`.
#[must_use]
pub fn from_env() -> Option<ExitCode> {
    let sock = std::env::var_os(askpass::SOCK)?;
    std::env::var_os("SSH_ASKPASS")?;
    let question = std::env::args().nth(1).unwrap_or_default();
    let kind = std::env::var(askpass::KIND).unwrap_or_default();
    let answer = match askpass::ask(std::path::Path::new(&sock), &kind, &question) {
        Ok(Some(answer)) => answer,
        Ok(None) => return Some(ExitCode::FAILURE),
        Err(e) => {
            eprintln!("slopty askpass: {e}");
            return Some(ExitCode::FAILURE);
        }
    };
    let mut out = std::io::stdout().lock();
    let wrote = out.write_all(answer.expose_secret()).and_then(|()| out.write_all(b"\n"));
    Some(if wrote.and_then(|()| out.flush()).is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
