//! One HTTP GET through the system's own URL loading (`NSURLSession`): its TLS, its proxy
//! settings and its network path, on macOS and iOS alike, with no HTTP stack of our own.
//!
//! For what is read rarely and off every hot path, such as the app's check for a newer release.

use std::fmt;

use block2::RcBlock;
use objc2_foundation::{
    NSData, NSError, NSHTTPURLResponse, NSString, NSURL, NSURLResponse, NSURLSession,
};
use parking_lot::Mutex;
use tokio::sync::oneshot;

/// Why a fetch brought no body.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FetchError {
    /// The address is not a URL.
    BadUrl,
    /// The system could not load it (no network, refused, TLS), in its words.
    Failed(String),
    /// The server answered with a status other than 200.
    Status(isize),
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadUrl => f.write_str("not a URL"),
            Self::Failed(why) => f.write_str(why),
            Self::Status(code) => write!(f, "the server answered {code}"),
        }
    }
}

impl std::error::Error for FetchError {}

/// The body `url` answers a GET with, when it answers 200.
///
/// # Errors
///
/// When the address is not a URL, the system cannot load it, or the server answers another
/// status.
pub async fn get(url: &str) -> Result<Vec<u8>, FetchError> {
    let asked = asked(url)?;
    asked.await.unwrap_or_else(|_dropped| Err(FetchError::Failed("no answer".to_owned())))
}

/// Start the load; its answer, once the session's handler runs. A plain function, so no
/// block is held across a wait and [`get`] stays `Send`.
fn asked(url: &str) -> Result<oneshot::Receiver<Result<Vec<u8>, FetchError>>, FetchError> {
    let address = NSURL::URLWithString(&NSString::from_str(url))
        .filter(|address| address.scheme().is_some() && address.host().is_some())
        .ok_or(FetchError::BadUrl)?;
    let (tx, rx) = oneshot::channel();
    let tx = Mutex::new(Some(tx));
    let handler = RcBlock::new(
        move |data: *mut NSData, response: *mut NSURLResponse, error: *mut NSError| {
            // SAFETY: Foundation rule: the handler's non-null arguments are valid
            // objects for the duration of the call.
            let data = unsafe { data.as_ref() };
            // SAFETY: as above.
            let response = unsafe { response.as_ref() };
            // SAFETY: as above.
            let error = unsafe { error.as_ref() };
            let answer = match (error, response.and_then(|r| r.downcast_ref::<NSHTTPURLResponse>()))
            {
                (Some(error), _) => {
                    Err(FetchError::Failed(error.localizedDescription().to_string()))
                }
                (None, Some(http)) if http.statusCode() != 200 => {
                    Err(FetchError::Status(http.statusCode()))
                }
                (None, _) => Ok(data.map(NSData::to_vec).unwrap_or_default()),
            };
            let tx = tx.lock().take();
            if let Some(tx) = tx {
                let _unheard = tx.send(answer);
            }
        },
    );
    // SAFETY: Foundation rule: `dataTaskWithURL:completionHandler:` copies the handler and
    // calls it once, on the session's delegate queue; it holds only `Send` data behind a
    // lock, as the method asks of a block it may call from any thread.
    let task = unsafe {
        NSURLSession::sharedSession().dataTaskWithURL_completionHandler(&address, &handler)
    };
    task.resume();
    Ok(rx)
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use super::*;

    /// A server on loopback that answers each connection with `reply`, whole.
    async fn serving(reply: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut peer, _)) = listener.accept().await {
                let mut request = [0_u8; 4096];
                let _read = peer.read(&mut request).await;
                let _sent = peer.write_all(reply.as_bytes()).await;
                let _shut = peer.shutdown().await;
            }
        });
        format!("http://{addr}/releases/latest")
    }

    /// A 200 brings its body; any other status, a refused connection or a string that is
    /// not a URL brings why.
    #[tokio::test]
    async fn a_get_brings_the_body_or_why_not() {
        let ok = serving(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 13\r\n\
             Connection: close\r\n\r\n{\"tag\":\"v1\"}\n",
        )
        .await;
        assert_eq!(get(&ok).await.unwrap(), b"{\"tag\":\"v1\"}\n");

        let missing =
            serving("HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await;
        assert_eq!(get(&missing).await, Err(FetchError::Status(404)));

        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let gone = format!("http://{}/", closed.local_addr().unwrap());
        drop(closed);
        assert!(matches!(get(&gone).await, Err(FetchError::Failed(_))));

        assert_eq!(get("not a url at all").await, Err(FetchError::BadUrl));
    }
}
