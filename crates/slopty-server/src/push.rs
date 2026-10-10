//! Pushing notices to pocketed phones (`slopty_proto::push`, `docs/decisions/platform.md`).
//!
//! The hub decides what to push (`hub::ladder`): a notice that finds the person at none of
//! their clients goes to every phone not listening on a live link. Here each one is sealed to
//! its phone ([`slopty_push::seal`]) and sent straight to APNs with the person's own key
//! ([`DirectPusher`]), over one HTTPS client ([`Https`]): rustls on ring, which builds for the
//! Linux server too, checking certificates as the system does. A phone APNs says is gone is
//! forgotten.
//!
//! A pushed ask answered on another client is taken back ([`Sending::TakeBack`]): a background
//! push naming the note's opaque collapse id, which iOS made the shown note's identifier.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper_rustls::HttpsConnector;
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use slopty_core::ClientId;
use slopty_proto::push::{PushBody, PushDevice};
use slopty_proto::thread::attention::{NoticeKind, Subject};
use slopty_push::apns::{self, Outcome};
use slopty_push::provider::ProviderKey;
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::hub::WeakHub;

/// How many pushes wait to be sealed and sent before more are dropped.
pub const QUEUE: usize = 64;
/// How long one request to APNs may take.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// How long to wait before each try again of a push APNs said to try later.
pub const RETRY_AFTER: [Duration; 2] = [Duration::from_secs(2), Duration::from_secs(10)];
/// How much of a notice's title, and of the subagent's it names, a push carries, in bytes.
pub const TITLE_BYTES: usize = 256;
/// How much of a notice's text a push carries, in bytes: with the title, the sealed body stays
/// well inside APNs' 4 KB once it is in base64.
pub const TEXT_BYTES: usize = 1536;

/// How the server pushes to phones.
#[derive(Clone, Debug, Default)]
pub enum PushConfig {
    /// It does not: nothing is set up.
    #[default]
    Off,
    /// Straight to APNs, with the person's own key.
    Direct(Arc<ProviderKey>),
    /// Through `pusher`, whatever it is.
    Through(Arc<dyn Pusher>),
}

impl PushConfig {
    /// Straight to APNs with the key in Apple's `.p8` (`pem`), its 10-character id and the
    /// team's.
    ///
    /// # Errors
    /// [`slopty_push::PushError::ProviderKey`] when `pem` is not a P-256 key in PKCS #8 PEM.
    pub fn direct(pem: &str, key_id: &str, team_id: &str) -> Result<Self, slopty_push::PushError> {
        Ok(Self::Direct(Arc::new(ProviderKey::from_p8(pem, key_id, team_id)?)))
    }
}

/// One push to make: a notice for a phone, not sealed yet, or notes to take back.
#[derive(Clone, Debug)]
pub struct Outgoing {
    /// The phone's client.
    pub client: ClientId,
    /// The phone.
    pub device: PushDevice,
    /// What it does there.
    pub what: Sending,
}

/// What a push does on the phone.
#[derive(Clone, PartialEq, Eq, Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "one per note a phone is pushed, a few a minute at most; a box would cost more"
)]
pub enum Sending {
    /// Show the note this body makes, sealed to the phone.
    Note(PushBody),
    /// Take back the notes pushed about these, by the collapse ids they were pushed under.
    TakeBack(Vec<Subject>),
}

/// What a [`Pusher`] returns.
pub type PushFuture<'a> = Pin<Box<dyn Future<Output = Outcome> + Send + 'a>>;

/// What sends a sealed push on to APNs.
pub trait Pusher: Send + Sync + fmt::Debug {
    /// Send `push` for the app `topic`, and say what came of it. A push that did not get there
    /// is [`Outcome::Later`].
    fn push<'a>(&'a self, push: &'a apns::Push, topic: &'a str) -> PushFuture<'a>;
}

/// Why pushing could not be set up.
#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    /// The system's certificates could not be used.
    #[error("tls: {0}")]
    Tls(#[from] rustls::Error),
    /// The certificate to trust is not one.
    #[error("the certificate to trust is not one")]
    Root,
}

/// An HTTPS client that speaks HTTP/2 only, as APNs does, checking certificates as the system
/// does.
#[derive(Clone)]
pub struct Https {
    client: Client<HttpsConnector<HttpConnector>, Full<Bytes>>,
}

impl fmt::Debug for Https {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Https").finish_non_exhaustive()
    }
}

/// Why a request got no answer.
#[derive(Debug, thiserror::Error)]
pub enum RequestError {
    /// The request is not one: a URL or a header that is not.
    #[error("{0}")]
    Request(#[from] hyper::http::Error),
    /// It did not get there: no connection, or TLS refused it.
    #[error("{0}")]
    Client(#[from] hyper_util::client::legacy::Error),
    /// The answer broke off.
    #[error("{0}")]
    Body(#[from] hyper::Error),
    /// No answer within [`REQUEST_TIMEOUT`].
    #[error("no answer in time")]
    Timeout,
}

impl Https {
    /// The client, trusting what the system trusts.
    ///
    /// # Errors
    /// [`SetupError::Tls`] when the system's verifier cannot be had.
    pub fn new() -> Result<Self, SetupError> {
        use rustls_platform_verifier::BuilderVerifierExt as _;
        let config = rustls::ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()?
            .with_platform_verifier()?
            .with_no_client_auth();
        Ok(Self::with(config))
    }

    /// The client, trusting only the certificate `root` (DER): a stand-in's, in its tests.
    ///
    /// # Errors
    /// [`SetupError::Root`] when `root` is not a certificate.
    pub fn trusting(root: Vec<u8>) -> Result<Self, SetupError> {
        let mut roots = rustls::RootCertStore::empty();
        roots
            .add(rustls::pki_types::CertificateDer::from(root))
            .map_err(|_bad| SetupError::Root)?;
        let config = rustls::ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()?
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(Self::with(config))
    }

    fn with(config: rustls::ClientConfig) -> Self {
        let connector = hyper_rustls::HttpsConnectorBuilder::new()
            .with_tls_config(config)
            .https_only()
            .enable_http2()
            .build();
        let client = Client::builder(TokioExecutor::new()).http2_only(true).build(connector);
        Self { client }
    }

    /// POST `body` to `url` with `headers`, and the answer's status and body.
    ///
    /// # Errors
    /// [`RequestError`] when no answer came.
    pub async fn post(
        &self,
        url: &str,
        headers: impl IntoIterator<Item = (&'static str, String)>,
        body: Vec<u8>,
    ) -> Result<(u16, Bytes), RequestError> {
        let mut request = hyper::Request::post(url);
        for (name, value) in headers {
            request = request.header(name, value);
        }
        let request = request.body(Full::new(Bytes::from(body)))?;
        let answer = async {
            let answer = self.client.request(request).await?;
            let status = answer.status().as_u16();
            let body = answer.into_body().collect().await?.to_bytes();
            Ok::<_, RequestError>((status, body))
        };
        tokio::time::timeout(REQUEST_TIMEOUT, answer)
            .await
            .map_err(|_late| RequestError::Timeout)?
    }
}

/// The crypto every client here uses: ring, which builds for musl with no C toolchain.
fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Seconds since the Unix epoch.
fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Pushes straight to APNs with the person's own key.
#[derive(Debug)]
pub struct DirectPusher {
    key: Arc<ProviderKey>,
    https: Https,
    origin: Option<String>,
}

impl DirectPusher {
    /// APNs, under `key`, over `https`.
    #[must_use]
    pub const fn new(key: Arc<ProviderKey>, https: Https) -> Self {
        Self { key, https, origin: None }
    }

    /// The same, sending to `origin` (`https://host:port`) in place of Apple's host: a
    /// stand-in's.
    #[must_use]
    pub fn at(self, origin: &str) -> Self {
        Self { origin: Some(origin.trim_end_matches('/').to_owned()), ..self }
    }
}

impl Pusher for DirectPusher {
    fn push<'a>(&'a self, push: &'a apns::Push, topic: &'a str) -> PushFuture<'a> {
        Box::pin(async move {
            let request = match apns::request(push, topic, &self.key.token(now())) {
                Ok(request) => request,
                Err(unfit) => return Outcome::Refused(unfit.to_string()),
            };
            let url = self
                .origin
                .as_ref()
                .map_or_else(|| request.url(), |origin| format!("{origin}{}", request.path));
            match self.https.post(&url, request.headers, request.body).await {
                Ok((status, body)) => apns::outcome(status, &body),
                Err(e) => {
                    tracing::debug!(error = %e, "APNs was not reached");
                    Outcome::Later
                }
            }
        })
    }
}

/// Why a push was not made.
#[derive(Debug, thiserror::Error)]
pub enum SealError {
    /// The body did not encode.
    #[error("{0}")]
    Encode(#[from] slopty_proto::codec::CodecError),
    /// It did not seal to the phone's key.
    #[error("{0}")]
    Seal(#[from] slopty_push::PushError),
}

/// `out` as APNs carries it: a body cut to fit and sealed to the phone, with the ids notes
/// stack and replace by, opaque and the phone's own; or a take-back naming those ids.
///
/// # Errors
/// [`SealError`] when the body does not encode or seal.
pub fn sealed(out: &Outgoing) -> Result<apns::Push, SealError> {
    let device = &out.device;
    let what = match &out.what {
        Sending::Note(body) => apns::What::Note(note(device, body)?),
        Sending::TakeBack(about) => apns::What::TakeBack(
            about.iter().map(|about| opaque(&device.key, &collapse_of(about))).collect(),
        ),
    };
    Ok(apns::Push { token: device.token.clone(), sandbox: device.sandbox, what })
}

/// The note `body` makes for `device`: cut to fit and sealed to it.
fn note(device: &PushDevice, body: &PushBody) -> Result<apns::Note, SealError> {
    let mut body = body.clone();
    cut(&mut body.notice.title, TITLE_BYTES);
    cut(&mut body.notice.text, TEXT_BYTES);
    if let Some(via) = &mut body.notice.via {
        cut(&mut via.title, TITLE_BYTES);
    }
    let bytes = slopty_proto::codec::encode_body(&body)?;
    let sealed = slopty_push::seal::seal(&device.key, &device.token, &bytes)?;
    let thread = match &body.notice.about {
        Subject::Thread(at) => format!("thread {} {}", at.worker, at.thread),
        Subject::Project { project, .. } => format!("project {project}"),
        Subject::Terminal(term) => format!("terminal {} {}", term.worker, term.session),
    };
    Ok(apns::Note {
        urgent: body.notice.kind == NoticeKind::NeedsYou,
        thread: opaque(&device.key, &thread),
        collapse: opaque(&device.key, &collapse_of(&body.notice.about)),
        sealed,
    })
}

/// What a note about `about` replaces, before it is made opaque: a thread's last note, a
/// terminal's program's, or a project's at one timeline entry.
fn collapse_of(about: &Subject) -> String {
    match about {
        Subject::Thread(at) => format!("thread {} {}", at.worker, at.thread),
        Subject::Project { project, entry } => format!("project {project} {entry}"),
        Subject::Terminal(term) => format!("terminal {} {}", term.worker, term.session),
    }
}

/// `what` as an id Apple sees: a hash keyed by the phone's key, so it names nothing and is the
/// same thing on no other phone.
fn opaque(key: &[u8; 32], what: &str) -> String {
    let hash = blake3::keyed_hash(key, what.as_bytes());
    hash.to_hex().chars().take(32).collect()
}

/// `text` cut to at most `bytes`, at a character, with an ellipsis where it was cut.
fn cut(text: &mut String, bytes: usize) {
    if text.len() <= bytes {
        return;
    }
    let ellipsis = '\u{2026}';
    let mut end = bytes.saturating_sub(ellipsis.len_utf8());
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.truncate(end);
    text.push(ellipsis);
}

/// The number of the latest push to each phone about each thing, by its collapse id's words
/// ([`collapse_of`]): a try again that a later push about the same thing overtook is given up.
#[derive(Debug, Default)]
struct Latest(parking_lot::Mutex<std::collections::HashMap<(ClientId, String), u64>>);

impl Latest {
    /// `out` is push number `n`: the latest about everything it names.
    fn mark(&self, out: &Outgoing, n: u64) {
        let mut latest = self.0.lock();
        for about in subjects(out) {
            latest.insert((out.client, collapse_of(about)), n);
        }
    }

    /// What of push number `n` is still the latest about what it names: all of it, part of a
    /// take-back, or none once later pushes overtook it.
    fn still(&self, out: &Outgoing, n: u64) -> Option<Outgoing> {
        let latest = self.0.lock();
        let mine = |about: &Subject| latest.get(&(out.client, collapse_of(about))) == Some(&n);
        let what = match &out.what {
            Sending::Note(body) => mine(&body.notice.about).then(|| Sending::Note(body.clone()))?,
            Sending::TakeBack(about) => {
                let left: Vec<Subject> = about.iter().filter(|a| mine(a)).cloned().collect();
                (!left.is_empty()).then_some(Sending::TakeBack(left))?
            }
        };
        Some(Outgoing { what, ..out.clone() })
    }

    /// Push number `n` is done: what it was the latest about is forgotten.
    fn done(&self, out: &Outgoing, n: u64) {
        let mut latest = self.0.lock();
        for about in subjects(out) {
            let key = (out.client, collapse_of(about));
            if latest.get(&key) == Some(&n) {
                latest.remove(&key);
            }
        }
    }
}

/// What `out` is about.
fn subjects(out: &Outgoing) -> Vec<&Subject> {
    match &out.what {
        Sending::Note(body) => vec![&body.notice.about],
        Sending::TakeBack(about) => about.iter().collect(),
    }
}

/// Seal and send every push `queue` brings through `pusher`, each on its own.
///
/// It runs until the queue closes and the last is sent. A phone APNs says is gone is
/// forgotten, and a push it says to try later is tried again ([`RETRY_AFTER`]), unless a later
/// push about the same thing came meanwhile: a note tried again after its take-back would show
/// again what was answered.
pub async fn deliver(hub: WeakHub, mut queue: mpsc::Receiver<Outgoing>, pusher: Arc<dyn Pusher>) {
    let mut sending = JoinSet::new();
    let latest = Arc::new(Latest::default());
    let mut next = 0_u64;
    while let Some(out) = queue.recv().await {
        while sending.try_join_next().is_some() {}
        next = next.wrapping_add(1);
        let n = next;
        latest.mark(&out, n);
        let (hub, pusher, latest) = (hub.clone(), Arc::clone(&pusher), Arc::clone(&latest));
        sending.spawn(async move {
            send(&hub, out.clone(), n, &latest, pusher.as_ref()).await;
            latest.done(&out, n);
        });
    }
    while sending.join_next().await.is_some() {}
}

/// Seal `out`, push number `n`, and send it through `pusher`, trying again while it is told to
/// and while it is the latest about what it names ([`Latest`]).
async fn send(hub: &WeakHub, mut out: Outgoing, n: u64, latest: &Latest, pusher: &dyn Pusher) {
    let mut waits = RETRY_AFTER.iter();
    loop {
        let push = match sealed(&out) {
            Ok(push) => push,
            Err(e) => {
                tracing::warn!(client = %out.client, error = %e, "a push was not sealed");
                return;
            }
        };
        match pusher.push(&push, &out.device.topic).await {
            Outcome::Sent => return,
            Outcome::Gone => {
                tracing::info!(client = %out.client, "APNs says a phone is gone; forgetting it");
                if let Some(hub) = hub.upgrade() {
                    hub.forget_device(out.client, &out.device.token);
                }
                return;
            }
            Outcome::Refused(why) => {
                tracing::warn!(client = %out.client, %why, "a push was refused");
                return;
            }
            Outcome::Later => {
                let Some(wait) = waits.next() else {
                    tracing::info!(client = %out.client, "a push did not go; giving it up");
                    return;
                };
                tokio::time::sleep(*wait).await;
                let Some(left) = latest.still(&out, n) else {
                    tracing::debug!(client = %out.client, "a push to try again was overtaken");
                    return;
                };
                out = left;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cut keeps whole characters and says it was cut; what fits is left alone.
    #[test]
    fn a_long_text_is_cut_at_a_character() {
        let mut short = "fits".to_owned();
        cut(&mut short, 8);
        assert_eq!(short, "fits");
        let mut long = "ééééé".to_owned();
        cut(&mut long, 6);
        assert_eq!(long, "é\u{2026}", "an ellipsis is three bytes, and half an é is none");
    }

    /// However long a notice's words, its push fits APNs' 4 KB, and its ids are opaque, the
    /// same for one thread and another for the next phone. A take-back names the note's own
    /// collapse id.
    #[test]
    fn a_long_notice_still_fits_apns() {
        use slopty_core::{SessionId, WorkerId};
        use slopty_proto::orchestration::TermRef;
        use slopty_proto::thread::ThreadId;
        use slopty_proto::thread::attention::{Notice, ThreadAt, Via};

        let worker = WorkerId::new();
        let notice = Notice {
            kind: NoticeKind::Finished,
            about: Subject::Thread(ThreadAt { worker, thread: ThreadId::new() }),
            tile: Some(TermRef { worker, session: SessionId::new() }),
            title: "\u{1f600}".repeat(500),
            text: "\u{1f600}".repeat(5_000),
            worked_ms: Some(90_000),
            via: Some(Via { thread: ThreadId::new(), title: "é".repeat(900) }),
        };
        let device = PushDevice {
            token: "ab".repeat(32),
            key: slopty_push::seal::DeviceKey::generate().unwrap().public(),
            sandbox: false,
            topic: "dev.aislopware.slopty".to_owned(),
            quiet_ms: 0,
        };
        let about = notice.about.clone();
        let out = Outgoing {
            client: ClientId::new(),
            device: device.clone(),
            what: Sending::Note(PushBody {
                notice,
                ask: None,
                choices: Vec::new(),
                quiet: false,
                merges: None,
            }),
        };
        let push = sealed(&out).unwrap();
        let request = apns::request(&push, &device.topic, "token").unwrap();
        assert!(request.body.len() <= apns::MAX_PAYLOAD, "{} bytes", request.body.len());
        let note = |push: apns::Push| match push.what {
            apns::What::Note(note) => note,
            apns::What::TakeBack(_) => panic!("a take-back"),
        };
        let shown = note(push);
        assert!(!shown.urgent);
        assert_eq!(note(sealed(&out).unwrap()).thread, shown.thread, "one thread, one stack");
        let other = Outgoing { device: PushDevice { key: [9; 32], ..device.clone() }, ..out };
        assert_ne!(note(sealed(&other).unwrap()).thread, shown.thread, "another phone, another id");

        let back = Outgoing { what: Sending::TakeBack(vec![about]), ..other };
        let back = Outgoing { device, ..back };
        let taken = sealed(&back).unwrap().what;
        assert_eq!(taken, apns::What::TakeBack(vec![shown.collapse]), "the note's own id");
    }

    /// A stand-in for APNs that says to try the first push again later, sends the rest, and
    /// keeps every push it was handed.
    #[derive(Debug, Default)]
    struct Busy(parking_lot::Mutex<Vec<apns::Push>>);

    impl Pusher for Busy {
        fn push<'a>(&'a self, push: &'a apns::Push, _topic: &'a str) -> PushFuture<'a> {
            Box::pin(async move {
                let mut seen = self.0.lock();
                seen.push(push.clone());
                if seen.len() == 1 { Outcome::Later } else { Outcome::Sent }
            })
        }
    }

    /// A note APNs said to try later is given up once its take-back went: tried again after
    /// it, it would show what was answered.
    #[tokio::test(start_paused = true)]
    async fn a_note_older_than_its_take_back_is_not_sent_again() {
        use slopty_core::{SessionId, WorkerId};
        use slopty_proto::orchestration::TermRef;
        use slopty_proto::thread::ThreadId;
        use slopty_proto::thread::attention::{Notice, ThreadAt};

        let worker = WorkerId::new();
        let about = Subject::Thread(ThreadAt { worker, thread: ThreadId::new() });
        let notice = Notice {
            kind: NoticeKind::NeedsYou,
            about: about.clone(),
            tile: Some(TermRef { worker, session: SessionId::new() }),
            title: "Allow?".to_owned(),
            text: "Bash".to_owned(),
            worked_ms: None,
            via: None,
        };
        let device = PushDevice {
            token: "ab".repeat(32),
            key: slopty_push::seal::DeviceKey::generate().unwrap().public(),
            sandbox: false,
            topic: "dev.aislopware.slopty".to_owned(),
            quiet_ms: 0,
        };
        let client = ClientId::new();
        let note = Outgoing {
            client,
            device: device.clone(),
            what: Sending::Note(PushBody {
                notice,
                ask: None,
                choices: Vec::new(),
                quiet: false,
                merges: None,
            }),
        };
        let back = Outgoing { client, device, what: Sending::TakeBack(vec![about]) };
        let hub = crate::Hub::new("server".to_owned(), Vec::new());
        let pusher = Arc::new(Busy::default());
        let (tx, rx) = mpsc::channel(4);
        tx.try_send(note).unwrap();
        tx.try_send(back).unwrap();
        drop(tx);
        deliver(hub.downgrade(), rx, Arc::<Busy>::clone(&pusher)).await;
        let seen: Vec<bool> =
            pusher.0.lock().iter().map(|p| matches!(p.what, apns::What::Note(_))).collect();
        assert_eq!(seen, [true, false], "the note once, then its take-back, and no note after");
    }
}
