//! The request APNs takes, and what its answer means.
//!
//! What it shows before the phone's extension runs is the server's fixed words ([`TITLE`],
//! [`URGENT`], [`NEWS`]), never the note's: those travel sealed under `e` and `s`, and the
//! extension (`mutable-content`) puts them in. If the extension fails or runs out of time, the
//! person still sees that an agent wants them. Only an agent that needs the person is Time
//! Sensitive, at the highest priority; the rest wait in the summary like any app's.
//!
//! A note answered elsewhere is taken back with a background push ([`What::TakeBack`]): no
//! alert, only the opaque ids the notes were pushed under ([`TAKE_BACK`]), which wake the app
//! for the moment it takes to remove them. iOS throttles such pushes and drops them for an app
//! the person force-quit, so the app's own sweep when it next comes to the front stays the one
//! that always holds.

use serde::Serialize;

use crate::seal::Sealed;

/// The generic alert's title.
pub const TITLE: &str = "Slopty";
/// The generic alert's body for an agent that needs the person.
pub const URGENT: &str = "An agent needs you";
/// The generic alert's body for anything else: a finished turn, a failure, a project's news.
pub const NEWS: &str = "An agent has news";

/// The most a payload may be, in bytes, as APNs takes it.
pub const MAX_PAYLOAD: usize = 4096;

/// The longest an opaque `thread-id` or collapse id may be, in characters: APNs takes 64 for a
/// collapse id.
pub const MAX_ID: usize = 64;

/// The payload key of a take-back's note ids: the collapse ids they were pushed under, which
/// iOS makes their identifiers.
pub const TAKE_BACK: &str = "w";
/// The most notes one take-back names.
pub const MAX_TAKE_BACK: usize = 16;

/// APNs in production.
pub const PRODUCTION: &str = "api.push.apple.com";
/// APNs for development builds.
pub const SANDBOX: &str = "api.sandbox.push.apple.com";

/// One push, as it goes to APNs: to whom, and what.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Push {
    /// The device token, in hex.
    pub token: String,
    /// A development build's token, which only the sandbox knows.
    pub sandbox: bool,
    /// A note to show, or notes to take back.
    pub what: What,
}

/// What a push does on the phone.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum What {
    /// Show a note.
    Note(Note),
    /// Take back the notes pushed under these collapse ids: a background push, shown nothing.
    TakeBack(Vec<String>),
}

/// A note to show: how urgent, its ids, and its sealed body.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Note {
    /// An agent that needs the person: Time Sensitive.
    pub urgent: bool,
    /// What notes stack under, opaque: a hash of the thread, never its name.
    pub thread: String,
    /// What a later push replaces, opaque: a hash of the note's id. iOS makes it the shown
    /// note's identifier, which a take-back names.
    pub collapse: String,
    /// The note, sealed to the device.
    pub sealed: Sealed,
}

/// An HTTP/2 request to APNs, for whatever client sends it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Request {
    /// The host: [`PRODUCTION`] or [`SANDBOX`].
    pub host: &'static str,
    /// The path: `/3/device/<token>`.
    pub path: String,
    /// The headers, by their lowercase names.
    pub headers: Vec<(&'static str, String)>,
    /// The JSON payload.
    pub body: Vec<u8>,
}

impl Request {
    /// The URL, for a client that takes one.
    #[must_use]
    pub fn url(&self) -> String {
        format!("https://{}{}", self.host, self.path)
    }
}

/// Why a push was not made into a request.
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
pub enum Unfit {
    /// The token is not a device token: hex, 64 to 200 characters.
    #[error("not a device token")]
    Token,
    /// An opaque id is not one: URL-safe base64, at most [`MAX_ID`] characters.
    #[error("not an opaque id")]
    Id,
    /// The payload would be over [`MAX_PAYLOAD`].
    #[error("too large for APNs")]
    TooLarge,
}

#[derive(Serialize)]
struct Payload<'a> {
    aps: Aps<'a>,
    e: String,
    s: String,
}

#[derive(Serialize)]
struct Background<'a> {
    aps: Wake,
    w: &'a [String],
}

#[derive(Serialize)]
struct Wake {
    #[serde(rename = "content-available")]
    content_available: u8,
}

#[derive(Serialize)]
struct Aps<'a> {
    alert: Alert<'a>,
    sound: &'a str,
    #[serde(rename = "mutable-content")]
    mutable_content: u8,
    #[serde(rename = "thread-id")]
    thread_id: &'a str,
    #[serde(rename = "interruption-level", skip_serializing_if = "Option::is_none")]
    interruption_level: Option<&'a str>,
}

#[derive(Serialize)]
struct Alert<'a> {
    title: &'a str,
    body: &'a str,
}

/// Whether `token` is a device token as APNs writes it.
#[must_use]
pub fn is_token(token: &str) -> bool {
    (64..=200).contains(&token.len()) && token.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Whether `id` is an opaque id as [`Push`] carries it.
#[must_use]
pub fn is_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// `push` as APNs takes it, under the provider token `bearer` for the app `topic`.
///
/// # Errors
/// [`Unfit`] when the token or an id is not one, a take-back names no note or more than
/// [`MAX_TAKE_BACK`], or the payload would be too large.
pub fn request(push: &Push, topic: &str, bearer: &str) -> Result<Request, Unfit> {
    if !is_token(&push.token) {
        return Err(Unfit::Token);
    }
    let (body, kind, priority, collapse) = match &push.what {
        What::Note(note) => {
            if !is_id(&note.thread) || !is_id(&note.collapse) {
                return Err(Unfit::Id);
            }
            let payload = Payload {
                aps: Aps {
                    alert: Alert { title: TITLE, body: if note.urgent { URGENT } else { NEWS } },
                    sound: "default",
                    mutable_content: 1,
                    thread_id: &note.thread,
                    interruption_level: note.urgent.then_some("time-sensitive"),
                },
                e: note.sealed.enc_text(),
                s: note.sealed.ct_text(),
            };
            let body = serde_json::to_vec(&payload).map_err(|_json| Unfit::TooLarge)?;
            let priority = if note.urgent { "10" } else { "5" };
            (body, "alert", priority, Some(note.collapse.clone()))
        }
        What::TakeBack(notes) => {
            if notes.is_empty() || notes.len() > MAX_TAKE_BACK || !notes.iter().all(|n| is_id(n)) {
                return Err(Unfit::Id);
            }
            let payload = Background { aps: Wake { content_available: 1 }, w: notes };
            let body = serde_json::to_vec(&payload).map_err(|_json| Unfit::TooLarge)?;
            // APNs refuses a background push at any other priority.
            (body, "background", "5", None)
        }
    };
    if body.len() > MAX_PAYLOAD {
        return Err(Unfit::TooLarge);
    }
    let mut headers = vec![
        ("authorization", format!("bearer {bearer}")),
        ("apns-topic", topic.to_owned()),
        ("apns-push-type", kind.to_owned()),
        ("apns-priority", priority.to_owned()),
    ];
    if let Some(collapse) = collapse {
        headers.push(("apns-collapse-id", collapse));
    }
    Ok(Request {
        host: if push.sandbox { SANDBOX } else { PRODUCTION },
        path: format!("/3/device/{}", push.token),
        headers,
        body,
    })
}

/// What APNs said of a push.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// Taken for delivery.
    Sent,
    /// The device is gone, or the token is not for this app: forget it.
    Gone,
    /// Busy or down, or the provider token is being renewed: try again later.
    Later,
    /// Refused, with APNs' reason.
    Refused(String),
}

/// What APNs' answer, `status` with `body`, means.
#[must_use]
pub fn outcome(status: u16, body: &[u8]) -> Outcome {
    let reason = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("reason").and_then(|r| r.as_str()).map(str::to_owned))
        .unwrap_or_default();
    match (status, reason.as_str()) {
        (200, _) => Outcome::Sent,
        (410, _) | (400, "BadDeviceToken" | "DeviceTokenNotForTopic") => Outcome::Gone,
        (429 | 500 | 503, _) | (403, "ExpiredProviderToken") => Outcome::Later,
        _ => Outcome::Refused(if reason.is_empty() { format!("status {status}") } else { reason }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(urgent: bool) -> Note {
        Note {
            urgent,
            thread: "t-1".to_owned(),
            collapse: "c_2".to_owned(),
            sealed: Sealed { enc: [7; 32], ct: vec![1, 2, 3] },
        }
    }

    fn push(urgent: bool) -> Push {
        Push { token: "ab".repeat(32), sandbox: false, what: What::Note(note(urgent)) }
    }

    /// Only an agent that needs the person is Time Sensitive at the highest priority; anything
    /// else is said in the other fixed words, at the low priority. A push over APNs' size, or
    /// with a token or an id that is not one, is no request.
    #[test]
    fn only_needs_you_is_time_sensitive() {
        let urgent = request(&push(true), "dev.aislopware.slopty", "jwt").unwrap();
        let json: serde_json::Value = serde_json::from_slice(&urgent.body).unwrap();
        assert_eq!(json["aps"]["interruption-level"], "time-sensitive");
        assert_eq!(json["aps"]["alert"]["body"], URGENT);
        assert_eq!(json["aps"]["mutable-content"], 1);
        assert!(urgent.headers.contains(&("apns-priority", "10".to_owned())));
        let news = request(&push(false), "dev.aislopware.slopty", "jwt").unwrap();
        let json: serde_json::Value = serde_json::from_slice(&news.body).unwrap();
        assert!(json["aps"].get("interruption-level").is_none());
        assert_eq!(json["aps"]["alert"]["body"], NEWS);
        assert!(news.headers.contains(&("apns-priority", "5".to_owned())));

        let mut large = note(true);
        large.sealed.ct = vec![0; MAX_PAYLOAD];
        let large = Push { what: What::Note(large), ..push(true) };
        assert_eq!(request(&large, "t", "jwt"), Err(Unfit::TooLarge));
        let mut token = push(true);
        token.token = "not hex".to_owned();
        assert_eq!(request(&token, "t", "jwt"), Err(Unfit::Token));
        let mut id = note(true);
        id.thread = "a thread's title".to_owned();
        let id = Push { what: What::Note(id), ..push(true) };
        assert_eq!(request(&id, "t", "jwt"), Err(Unfit::Id));
    }

    /// A take-back is a background push: no alert, no sound and no collapse id, at the low
    /// priority APNs demands of one, naming the notes' collapse ids and nothing else. One that
    /// names no note, too many, or an id that is not one is no request.
    #[test]
    fn a_take_back_wakes_the_app_and_shows_nothing() {
        let notes = vec!["c_2".to_owned(), "c_3".to_owned()];
        let back = Push { what: What::TakeBack(notes.clone()), ..push(true) };
        let asked = request(&back, "dev.aislopware.slopty", "jwt").unwrap();
        assert!(asked.headers.contains(&("apns-push-type", "background".to_owned())));
        assert!(asked.headers.contains(&("apns-priority", "5".to_owned())));
        assert!(asked.headers.iter().all(|(name, _)| *name != "apns-collapse-id"));
        let json: serde_json::Value = serde_json::from_slice(&asked.body).unwrap();
        assert_eq!(json, serde_json::json!({ "aps": { "content-available": 1 }, "w": notes }));

        let none = Push { what: What::TakeBack(Vec::new()), ..push(true) };
        assert_eq!(request(&none, "t", "jwt"), Err(Unfit::Id));
        let many = vec!["c".to_owned(); MAX_TAKE_BACK + 1];
        let many = Push { what: What::TakeBack(many), ..push(true) };
        assert_eq!(request(&many, "t", "jwt"), Err(Unfit::Id));
        let odd = Push { what: What::TakeBack(vec!["a title".to_owned()]), ..push(true) };
        assert_eq!(request(&odd, "t", "jwt"), Err(Unfit::Id));
    }

    /// APNs' answers: taken, a device gone, a reason to try later, a refusal with its reason.
    #[test]
    fn apns_answers_say_what_to_do_next() {
        assert_eq!(outcome(200, b""), Outcome::Sent);
        assert_eq!(outcome(410, br#"{"reason":"Unregistered"}"#), Outcome::Gone);
        assert_eq!(outcome(400, br#"{"reason":"BadDeviceToken"}"#), Outcome::Gone);
        assert_eq!(outcome(429, br#"{"reason":"TooManyRequests"}"#), Outcome::Later);
        assert_eq!(outcome(403, br#"{"reason":"ExpiredProviderToken"}"#), Outcome::Later);
        let refused = outcome(400, br#"{"reason":"PayloadTooLarge"}"#);
        assert_eq!(refused, Outcome::Refused("PayloadTooLarge".to_owned()));
        assert_eq!(outcome(502, b"bad gateway"), Outcome::Refused("status 502".to_owned()));
    }
}
