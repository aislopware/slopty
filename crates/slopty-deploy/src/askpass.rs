//! A password the person typed, handed to the system `ssh` through OpenSSH's own askpass door
//! and nowhere else.
//!
//! `ssh` runs the program `SSH_ASKPASS` names for every question it would ask at a terminal
//! (`SSH_ASKPASS_REQUIRE=force`), with the question as its argument, and reads the answer from
//! its standard output. That program is the `slopty` CLI ([`SOCK`] in its environment turns it
//! into the helper): it passes the question to a socket the deploy serves and prints
//! what comes back ([`ask`]). The socket answers one password question once, with the password,
//! and nothing else: a yes-or-no question (a host key to accept) and every question after the
//! first are refused, which `ssh` takes as the person saying no.
//!
//! The password never sits in an argument, the environment, a file or a log: it goes from the
//! [`SecretString`] the sheet filled, over the socket, to the helper, which writes it to the pipe
//! `ssh` reads. Only the socket's path is in the environment, and only this user may open the
//! directory it is in.

use std::io::{self, Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use secrecy::{ExposeSecret as _, SecretSlice, SecretString};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// The socket the helper asks, in the environment of the `ssh` that signs in and nowhere else.
pub const SOCK: &str = "SLOPTY_ASKPASS_SOCK";

/// What `ssh` sets for a question that is not a password's: `confirm` for a yes or no, `none`
/// for a note with nothing to answer.
pub const KIND: &str = "SSH_ASKPASS_PROMPT";

/// The longest question read: `ssh`'s are a line.
const QUESTION_MAX: u64 = 4096;

/// The longest answer the helper takes.
const ANSWER_MAX: u64 = 64 * 1024;

/// How long the helper waits for the answer: the socket answers at once, or is gone.
const WAIT: Duration = Duration::from_secs(30);

/// Ask the socket at `sock` the question `ssh` asked: `kind` as [`KIND`] said it (empty for a
/// password's), `question` as `ssh` worded it. The answer, or `None` when it refused.
///
/// # Errors
///
/// When the socket is not there or does not answer in time.
pub fn ask(sock: &Path, kind: &str, question: &str) -> io::Result<Option<SecretSlice<u8>>> {
    let mut stream = UnixStream::connect(sock)?;
    stream.set_read_timeout(Some(WAIT))?;
    stream.set_write_timeout(Some(WAIT))?;
    let kind = kind.replace('\n', " ");
    stream.write_all(format!("{kind}\n{question}").as_bytes())?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut answer = Vec::new();
    stream.take(ANSWER_MAX).read_to_end(&mut answer)?;
    Ok((!answer.is_empty()).then(|| SecretSlice::from(answer)))
}

/// The socket answering a sign-in's questions, until it is dropped.
#[derive(Debug)]
pub(crate) struct Answering(tokio::task::JoinHandle<()>);

impl Drop for Answering {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Answer the first password question asked at `sock` with `password`, and refuse every other
/// question, until the returned handle is dropped.
pub(crate) fn serve(sock: &Path, password: SecretString) -> io::Result<Answering> {
    let listener = tokio::net::UnixListener::bind(sock)?;
    let task = tokio::spawn(async move {
        let mut password = Some(password);
        loop {
            let Ok((mut stream, _)) = listener.accept().await else { return };
            let mut asked = Vec::new();
            let mut question = (&mut stream).take(QUESTION_MAX);
            if tokio::time::timeout(WAIT, question.read_to_end(&mut asked)).await.is_err() {
                continue;
            }
            let asked = String::from_utf8_lossy(&asked);
            let kind = asked.split_once('\n').map_or_else(|| asked.as_ref(), |(kind, _)| kind);
            let answer = match kind {
                "confirm" | "none" => None,
                _ => password.take(),
            };
            tracing::debug!(kind, answered = answer.is_some(), "askpass");
            if let Some(answer) = answer {
                let wrote = stream.write_all(answer.expose_secret().as_bytes()).await;
                if let Err(e) = wrote {
                    tracing::warn!(error = %e, "answer ssh's askpass");
                }
            }
            drop(stream);
        }
    });
    Ok(Answering(task))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The socket answers the first password question with the password and nothing after;
    /// a yes-or-no question is refused, and so is a question at a socket no longer served.
    #[tokio::test]
    async fn the_password_answers_one_question_once() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("a");
        let served = serve(&sock, SecretString::from("hunter2")).unwrap();
        let asked = |kind: &'static str| {
            let sock = sock.clone();
            tokio::task::spawn_blocking(move || ask(&sock, kind, "me@mini's password: "))
        };
        let confirm = asked("confirm").await.unwrap().unwrap();
        assert!(confirm.is_none(), "a host key is never accepted from here");
        let first = asked("").await.unwrap().unwrap().unwrap();
        assert_eq!(first.expose_secret(), b"hunter2");
        assert!(asked("").await.unwrap().unwrap().is_none(), "once");
        drop(served);
        tokio::task::yield_now().await;
        let gone = asked("").await.unwrap();
        assert!(gone.is_err() || gone.is_ok_and(|a| a.is_none()), "nothing answers");
    }
}
