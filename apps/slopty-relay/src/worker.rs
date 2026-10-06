//! The Worker: the request in, `slopty_push::relay`'s checks, the binding, the limits, and
//! the push out to APNs, whose answer goes back as it came.

use slopty_push::provider::ProviderKey;
use slopty_push::relay::{self, Refusal};
use worker::{Context, Env, Fetch, Headers, Method, Request, RequestInit, Response, event};

use crate::binding;

/// The KV namespace the bindings live in.
const BINDINGS: &str = "BINDINGS";
/// The limiter keyed by the phone's token.
const DEVICE_LIMIT: &str = "DEVICE_LIMIT";
/// The limiter keyed by the install's key.
const INSTALL_LIMIT: &str = "INSTALL_LIMIT";
/// The app's bundle identifier, APNs' topic: a plain variable.
const TOPIC: &str = "APNS_TOPIC";
/// The team's APNs key, in the `.p8`'s PEM, and its ids: secrets.
const KEY: &str = "APNS_KEY";
const KEY_ID: &str = "APNS_KEY_ID";
const TEAM_ID: &str = "APNS_TEAM_ID";

#[event(fetch)]
async fn fetch(mut request: Request, env: Env, _ctx: Context) -> worker::Result<Response> {
    if request.path() != relay::PATH {
        return Response::error("not found", 404);
    }
    if request.method() != Method::Post {
        return Response::error("POST only", 405);
    }
    let length = request.headers().get("content-length")?.and_then(|l| l.parse::<usize>().ok());
    if length.is_some_and(|l| l > relay::MAX_BODY) {
        return refuse(Refusal::TooLarge);
    }
    let body = request.bytes().await?;
    let headers = request.headers();
    let (key, at, signature) = (
        headers.get(relay::KEY_HEADER)?,
        headers.get(relay::AT_HEADER)?,
        headers.get(relay::SIGNATURE_HEADER)?,
    );
    let incoming = relay::Incoming {
        key: key.as_deref(),
        at: at.as_deref(),
        signature: signature.as_deref(),
        body: &body,
    };
    let now = worker::Date::now().as_millis() / 1000;
    let admitted = match relay::admit(&incoming, now) {
        Ok(admitted) => admitted,
        Err(refusal) => return refuse(refusal),
    };
    let install = relay::key_text(&admitted.key);
    for (limiter, key) in [(INSTALL_LIMIT, &install), (DEVICE_LIMIT, &admitted.push.token)] {
        if !env.rate_limiter(limiter)?.limit(key.clone()).await?.success {
            return refuse(Refusal::Limited);
        }
    }
    let bindings = env.kv(BINDINGS)?;
    let stored = bindings.get(&admitted.push.token).text().await?;
    match binding::bind(stored.as_deref(), admitted.key, now) {
        Ok(Some(record)) => {
            bindings
                .put(&admitted.push.token, record)?
                .expiration_ttl(binding::TTL_SECONDS)
                .execute()
                .await?;
        }
        Ok(None) => {}
        Err(refusal) => return refuse(refusal),
    }
    let provider = ProviderKey::from_p8(
        &env.secret(KEY)?.to_string(),
        &env.secret(KEY_ID)?.to_string(),
        &env.secret(TEAM_ID)?.to_string(),
    )
    .map_err(|e| worker::Error::RustError(e.to_string()))?;
    let topic = env.var(TOPIC)?.to_string();
    let to_apns = match relay::forward(&admitted, &provider, &topic, now) {
        Ok(to_apns) => to_apns,
        Err(refusal) => return refuse(refusal),
    };
    let out = Headers::new();
    for (name, value) in &to_apns.headers {
        out.set(name, value)?;
    }
    let mut init = RequestInit::new();
    let body = worker::js_sys::Uint8Array::from(to_apns.body.as_slice());
    init.with_method(Method::Post).with_headers(out).with_body(Some(body.into()));
    let mut answer = Fetch::Request(Request::new_with_init(&to_apns.url(), &init)?).send().await?;
    let status = answer.status_code();
    Ok(Response::from_bytes(answer.bytes().await?)?.with_status(status))
}

/// The answer for `refusal`: its status, with no body.
fn refuse(refusal: Refusal) -> worker::Result<Response> {
    Ok(Response::empty()?.with_status(refusal.status()))
}
