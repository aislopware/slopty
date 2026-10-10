//! A notice reaches a pocketed phone: a worker's thread comes to need the person while their
//! phone has stopped listening, the server seals the notice to the phone's key and sends it
//! through a stand-in relay running the relay's own checks, which forwards it to a stand-in
//! APNs; and, for a self-builder, straight to that APNs with their own key. Both stand-ins
//! speak HTTP/2 over TLS on loopback, as the real ones do. What APNs got opens with the phone's
//! key to the notice's words, and carries none of them in the clear. Once the ask is answered
//! elsewhere, a background push takes the note back by the id APNs showed it under.

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashMap};
    use std::convert::Infallible;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use bytes::Bytes;
    use http_body_util::{BodyExt as _, Full};
    use hyper::body::Incoming;
    use hyper::service::service_fn;
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use parking_lot::Mutex;
    use slopty_core::{ClientId, SessionId, WallMs, WorkerId};
    use slopty_net::HostAddr;
    use slopty_net::admission::Admission;
    use slopty_net::client::bind_client;
    use slopty_net::server::{ServerLink, connect};
    use slopty_proto::push::{PushBody, PushDevice};
    use slopty_proto::server::{FromServer, Os, Registration, Role, ToServer, WorkerCaps};
    use slopty_proto::terminal::{SessionState, SessionSummary};
    use slopty_proto::thread::attention::{NoticeKind, Presence, Seat};
    use slopty_proto::thread::wire::{RequestCard, TableFrame, ThreadRow};
    use slopty_proto::thread::{
        AgentId, AskId, Changed, Choice, Cursor, Drive, Effect, Liveness, Meters, Phase, Request,
        Status, ThreadId,
    };
    use slopty_push::provider::ProviderKey;
    use slopty_push::relay::{self, InstallKey, PublicKey};
    use slopty_push::seal::{DeviceKey, Sealed};
    use slopty_server::push::{DirectPusher, Https, Pusher, RelayPusher};
    use slopty_server::{Config, PushConfig, Server};
    use tokio::sync::mpsc;

    const PATIENCE: Duration = Duration::from_secs(10);
    /// The app the relay pushes for, as its own setting names it.
    const RELAY_TOPIC: &str = "dev.aislopware.slopty";
    /// The app the phone says it is.
    const PHONE_TOPIC: &str = "dev.aislopware.slopty.dev";

    /// One request a stand-in got.
    #[derive(Debug)]
    struct Got {
        path: String,
        headers: HashMap<String, String>,
        body: Bytes,
    }

    /// What a stand-in answers a request with: a status and a body.
    type Answer = std::pin::Pin<Box<dyn Future<Output = (u16, Bytes)> + Send>>;

    /// An HTTP/2-over-TLS server on loopback answering each request with `answer`: its origin
    /// and its certificate, which the client is to trust.
    async fn stand_in(answer: impl Fn(Got) -> Answer + Send + Sync + 'static) -> (String, Vec<u8>) {
        let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let der = certified.cert.der().to_vec();
        let key =
            rustls::pki_types::PrivateKeyDer::Pkcs8(certified.signing_key.serialize_der().into());
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![der.clone().into()], key)
            .unwrap();
        config.alpn_protocols = vec![b"h2".to_vec()];
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("https://localhost:{}", listener.local_addr().unwrap().port());
        let answer = Arc::new(answer);
        tokio::spawn(async move {
            loop {
                let Ok((tcp, _)) = listener.accept().await else { return };
                let (acceptor, answer) = (acceptor.clone(), Arc::clone(&answer));
                tokio::spawn(async move {
                    let Ok(tls) = acceptor.accept(tcp).await else { return };
                    let service = service_fn(move |request: hyper::Request<Incoming>| {
                        let answer = Arc::clone(&answer);
                        async move {
                            let path = request.uri().path().to_owned();
                            let headers = request
                                .headers()
                                .iter()
                                .map(|(k, v)| {
                                    (k.as_str().to_owned(), v.to_str().unwrap().to_owned())
                                })
                                .collect();
                            let body = request.into_body().collect().await.unwrap().to_bytes();
                            let (status, body) = answer(Got { path, headers, body }).await;
                            let response = hyper::Response::builder()
                                .status(status)
                                .body(Full::new(body))
                                .unwrap();
                            Ok::<_, Infallible>(response)
                        }
                    });
                    let _ended = hyper::server::conn::http2::Builder::new(TokioExecutor::new())
                        .serve_connection(TokioIo::new(tls), service)
                        .await;
                });
            }
        });
        (origin, der)
    }

    /// A stand-in APNs that takes every push and hands it on `got`.
    async fn apns() -> (String, Vec<u8>, mpsc::UnboundedReceiver<Got>) {
        let (tx, got) = mpsc::unbounded_channel();
        let (origin, der) = stand_in(move |request| {
            let _heard = tx.send(request);
            Box::pin(std::future::ready((200, Bytes::new())))
        })
        .await;
        (origin, der, got)
    }

    fn now() -> u64 {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
    }

    /// A stand-in relay: the relay's own checks and binding (`slopty_push::relay`), then the
    /// push to APNs at `apns` under `provider`'s token, whose answer it gives back.
    async fn relay(provider: ProviderKey, apns: String, https: Https) -> (String, Vec<u8>) {
        let bound: Arc<Mutex<BTreeMap<String, Vec<PublicKey>>>> = Arc::default();
        let provider = Arc::new(provider);
        stand_in(move |request| {
            let (bound, provider, apns, https) =
                (Arc::clone(&bound), Arc::clone(&provider), apns.clone(), https.clone());
            Box::pin(async move {
                assert_eq!(request.path, relay::PATH);
                let header = |name: &str| request.headers.get(name).map(String::as_str);
                let incoming = relay::Incoming {
                    key: header(relay::KEY_HEADER),
                    at: header(relay::AT_HEADER),
                    signature: header(relay::SIGNATURE_HEADER),
                    body: &request.body,
                };
                let admitted = match relay::admit(&incoming, now()) {
                    Ok(admitted) => admitted,
                    Err(refusal) => return (refusal.status(), Bytes::new()),
                };
                let token = admitted.push.token.clone();
                let keys = bound.lock().get(&token).cloned().unwrap_or_default();
                match relay::bind(admitted.key, &keys) {
                    Ok(keys) => bound.lock().insert(token, keys),
                    Err(refusal) => return (refusal.status(), Bytes::new()),
                };
                let to_apns = match relay::forward(&admitted, &provider, RELAY_TOPIC, now()) {
                    Ok(to_apns) => to_apns,
                    Err(refusal) => return (refusal.status(), Bytes::new()),
                };
                let url = format!("{apns}{}", to_apns.path);
                https.post(&url, to_apns.headers, to_apns.body).await.unwrap()
            })
        })
        .await
    }

    fn summary(id: SessionId) -> SessionSummary {
        SessionSummary {
            id,
            title: "claude".to_owned(),
            cwd: None,
            repo: None,
            branch: None,
            changes: None,
            started_ms: WallMs::ZERO,
            cols: 80,
            rows: 24,
            state: SessionState::Running,
            viewers: 0,
            command: Vec::new(),
            progress: None,
            restored: None,
            program: Vec::new(),
            repo_id: None,
        }
    }

    fn row(id: ThreadId, phase: Phase, since: u64, terminal: SessionId) -> ThreadRow {
        ThreadRow {
            id,
            agent: AgentId::named(AgentId::CLAUDE_CODE),
            title: "Rank the fleet".to_owned(),
            status: Status {
                phase,
                wait: None,
                liveness: Liveness::Live,
                since_ms: WallMs::from_millis(since),
            },
            requests: Vec::new(),
            last_line: None,
            doing: None,
            changed: Changed::default(),
            terminal: Some(terminal),
            parent: None,
            drive: Drive::named(Drive::OBSERVED),
            caps: Vec::new(),
            facts: BTreeMap::new(),
            to_review: false,
            pull: None,
            meters: Meters::default(),
            ended: None,
            seen: slopty_proto::thread::TurnId::BEFORE,
            draft: None,
            updated_ms: WallMs::from_millis(since),
            cwd: None,
            repo: None,
            repo_id: None,
        }
    }

    async fn dial(server: &Server, role: Role) -> ServerLink {
        let endpoint = bind_client().unwrap();
        let addr = HostAddr::from(server.quic_addr());
        tokio::time::timeout(PATIENCE, connect(&endpoint, &addr, role)).await.unwrap().unwrap()
    }

    /// A server pushing through `pusher`; a phone that registers with it and then stops
    /// listening; a worker whose thread comes to need the person for a yes or no, which is
    /// then answered elsewhere. What APNs got, read off `got`: the note and its take-back, with
    /// the phone's key and token.
    async fn a_phone_is_pushed(
        pusher: Arc<dyn Pusher>,
        got: &mut mpsc::UnboundedReceiver<Got>,
    ) -> (Got, Got, DeviceKey, String) {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::start(Config {
            name: "test-server".to_owned(),
            quic: "127.0.0.1:0".parse().unwrap(),
            data_dir: dir.path().to_path_buf(),
            admission: Admission::with_tailnet(Vec::new(), None),
            push: PushConfig::Through(pusher),
        })
        .await
        .unwrap();

        let key = DeviceKey::generate().unwrap();
        let token = "0f".repeat(32);
        let device = PushDevice {
            token: token.clone(),
            key: key.public(),
            sandbox: true,
            topic: PHONE_TOPIC.to_owned(),
            quiet_ms: 30_000,
        };
        let mut phone = dial(&server, Role::Client { name: "phone".to_owned() }).await;
        let client = ClientId::new();
        let push_device = ToServer::PushDevice { client, device: Some(device) };
        phone.tx.send(&push_device).await.unwrap();
        let away = Presence {
            seat: Seat::Handheld,
            active: false,
            showing: Vec::new(),
            focus: None,
            listening: false,
        };
        phone.tx.send(&ToServer::Presence(away)).await.unwrap();
        loop {
            let msg = tokio::time::timeout(PATIENCE, phone.rx.recv()).await.unwrap().unwrap();
            if matches!(&msg, FromServer::Present(list) if list.iter().any(|p| !p.presence.listening))
            {
                break;
            }
        }
        assert_eq!(server.hub().devices().len(), 1, "the phone is registered");

        let (worker, shell) = (WorkerId::new(), SessionId::new());
        let registration = Registration {
            worker,
            name: "fake-worker".to_owned(),
            listen: std::net::SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, 45999)),
            caps: WorkerCaps::bare(Os::MacOs),
            sessions: vec![summary(shell)],
            session_key: [7; 32],
        };
        let mut link = dial(&server, Role::Worker(Box::new(registration))).await;
        let thread = ThreadId::new();
        let rows = vec![row(thread, Phase::Working, 1_000, shell)];
        let snapshot = TableFrame::Snapshot { cursor: Cursor::default(), rows };
        link.tx.send(&ToServer::Threads(snapshot)).await.unwrap();
        let choice = |id: &str, effect| Choice {
            id: id.to_owned(),
            label: id.to_owned(),
            effect,
            scope: None,
            stops: false,
        };
        let mut asking = row(thread, Phase::NeedsYou, 2_000, shell);
        asking.requests.push(RequestCard {
            id: AskId("toolu_01".to_owned()),
            item: None,
            kind: Request::APPROVAL.to_owned(),
            title: "Run cargo test?".to_owned(),
            options: vec![choice("yes", Effect::Allow), choice("no", Effect::Deny)],
            buttons: Vec::new(),
            opened_ms: WallMs::from_millis(2_000),
        });
        // Once the working row is ranked, so the next is a climb and not a first sight.
        while server.hub().ladder().threads.is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let delta =
            TableFrame::Delta { cursor: Cursor::default(), rows: vec![asking], removed: vec![] };
        link.tx.send(&ToServer::Threads(delta)).await.unwrap();
        let pushed = tokio::time::timeout(PATIENCE, got.recv()).await.unwrap().unwrap();
        let answered = row(thread, Phase::Working, 3_000, shell);
        let delta =
            TableFrame::Delta { cursor: Cursor::default(), rows: vec![answered], removed: vec![] };
        link.tx.send(&ToServer::Threads(delta)).await.unwrap();
        let taken_back = tokio::time::timeout(PATIENCE, got.recv()).await.unwrap().unwrap();
        server.shutdown().await;
        (pushed, taken_back, key, token)
    }

    /// What APNs got once the ask was answered elsewhere: a background push to the same phone,
    /// showing nothing, naming the id the note was shown under and nothing else.
    fn takes_the_note_back(got: &Got, note: &Got, topic: &str) {
        assert_eq!(got.path, note.path);
        let header = |name: &str| got.headers.get(name).map(String::as_str);
        assert_eq!(header("apns-topic"), Some(topic));
        assert_eq!(header("apns-push-type"), Some("background"));
        assert_eq!(header("apns-priority"), Some("5"));
        let shown_as = &note.headers["apns-collapse-id"];
        let payload: serde_json::Value = serde_json::from_slice(&got.body).unwrap();
        assert_eq!(
            payload,
            serde_json::json!({ "aps": { "content-available": 1 }, "w": [shown_as] })
        );
    }

    /// What APNs got is an urgent alert to the phone's token in fixed words, with the notice
    /// sealed: it opens with the phone's key to the notice and the ask its buttons answer, and
    /// says none of its words in the clear.
    fn opens_to_the_notice(got: &Got, key: &DeviceKey, token: &str, topic: &str) {
        assert_eq!(got.path, format!("/3/device/{token}"));
        let header = |name: &str| got.headers.get(name).map(String::as_str);
        assert_eq!(header("apns-topic"), Some(topic));
        assert_eq!(header("apns-push-type"), Some("alert"));
        assert_eq!(header("apns-priority"), Some("10"), "needing the person is urgent");
        assert!(header("authorization").is_some_and(|a| a.starts_with("bearer ")));
        let clear = String::from_utf8(got.body.to_vec()).unwrap();
        assert!(!clear.contains("cargo") && !clear.contains("Rank"), "no word in the clear");
        let payload: serde_json::Value = serde_json::from_slice(&got.body).unwrap();
        let aps = &payload["aps"];
        assert_eq!(aps["alert"]["title"], slopty_push::apns::TITLE);
        assert_eq!(aps["alert"]["body"], slopty_push::apns::URGENT);
        assert_eq!(aps["interruption-level"], "time-sensitive");
        assert_eq!(aps["mutable-content"], 1);
        let sealed =
            Sealed::from_text(payload["e"].as_str().unwrap(), payload["s"].as_str().unwrap())
                .unwrap();
        let opened = key.open(token, &sealed).unwrap();
        let body: PushBody = slopty_proto::codec::decode_body(&opened).unwrap();
        assert_eq!(body.notice.kind, NoticeKind::NeedsYou);
        assert_eq!(
            (body.notice.title.as_str(), body.notice.text.as_str()),
            ("Rank the fleet", "Run cargo test?")
        );
        assert_eq!(body.ask, Some(AskId("toolu_01".to_owned())));
        assert!(key.open(&"1f".repeat(32), &sealed).is_err(), "bound to its own token");
    }

    #[tokio::test]
    async fn a_notice_reaches_the_phone_through_the_relay_and_is_taken_back() {
        let (apns_origin, apns_der, mut got) = apns().await;
        let pem =
            rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap().serialize_pem();
        let provider = || ProviderKey::from_p8(&pem, "ABC123DEFG", "DEF456GHIJ").unwrap();

        let to_apns = Https::trusting(apns_der.clone()).unwrap();
        let (relay_origin, relay_der) = relay(provider(), apns_origin.clone(), to_apns).await;
        let install = InstallKey::generate().unwrap();
        let https = Https::trusting(relay_der).unwrap();
        let through = Arc::new(RelayPusher::new(&relay_origin, install, https));
        let (pushed, back, key, token) = a_phone_is_pushed(through, &mut got).await;
        opens_to_the_notice(&pushed, &key, &token, RELAY_TOPIC);
        takes_the_note_back(&back, &pushed, RELAY_TOPIC);

        // A self-builder's server goes straight to APNs, for the app the phone names.
        let https = Https::trusting(apns_der).unwrap();
        let direct = Arc::new(DirectPusher::new(Arc::new(provider()), https).at(&apns_origin));
        let (pushed, back, key, token) = a_phone_is_pushed(direct, &mut got).await;
        opens_to_the_notice(&pushed, &key, &token, PHONE_TOPIC);
        takes_the_note_back(&back, &pushed, PHONE_TOPIC);
    }
}
