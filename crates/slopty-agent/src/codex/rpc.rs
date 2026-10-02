//! JSON-RPC as the app-server speaks it: JSON-RPC 2.0 without the `"jsonrpc"` field, one
//! message to a WebSocket text frame.
//!
//! Both sides send requests. Slopty's ([`request`]) are answered by id ([`Incoming::Answer`]);
//! the app-server's ([`Incoming::Request`]: approvals and questions) are answered by Slopty
//! ([`answer`]), and one these types do not name is refused ([`refuse`]) so the turn waiting on
//! it is not left hanging. A notification these types do not name is passed over.

use std::fmt;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use super::protocol::{Method, RequestId, ServerNotification, ServerRequest};

/// JSON-RPC's code for a method the receiver does not have.
pub const METHOD_NOT_FOUND: i64 = -32601;

/// A message from the app-server.
#[derive(Clone, PartialEq, Debug)]
pub enum Incoming {
    /// The answer to request `id`.
    Answer {
        /// The request.
        id: RequestId,
        /// What it gave back, or why it failed.
        outcome: Result<Value, RpcError>,
    },
    /// A request for Slopty to answer.
    Request {
        /// Its id, which the answer carries.
        id: RequestId,
        /// The thread it is about, as its params name it.
        thread: Option<String>,
        /// What it asks.
        request: Box<ServerRequest>,
    },
    /// A request these types do not name.
    UnknownRequest {
        /// Its id.
        id: RequestId,
        /// Its method.
        method: String,
    },
    /// A notification.
    Notification {
        /// The thread it is about, as its params name it.
        thread: Option<String>,
        /// What it says.
        note: Box<ServerNotification>,
    },
    /// A notification these types do not name.
    UnknownNotification {
        /// Its method.
        method: String,
    },
}

/// Why a request failed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RpcError {
    /// JSON-RPC's code.
    pub code: i64,
    /// What went wrong, in words.
    pub message: String,
    /// More, as the app-server gives it.
    pub data: Option<Value>,
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.message, self.code)
    }
}

impl std::error::Error for RpcError {}

/// A frame that is no message these types read.
#[derive(Debug)]
pub enum ReadError {
    /// It is not JSON, or its params or result are not what its method takes.
    Json(serde_json::Error),
    /// It is JSON but no JSON-RPC message.
    Shape(String),
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(e) => write!(f, "{e}"),
            Self::Shape(what) => write!(f, "no JSON-RPC message: {what}"),
        }
    }
}

impl std::error::Error for ReadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(e) => Some(e),
            Self::Shape(_) => None,
        }
    }
}

impl From<serde_json::Error> for ReadError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

/// Read one frame.
///
/// # Errors
///
/// When the frame is not JSON, is no JSON-RPC message, or holds params a named method does
/// not take.
pub fn read(frame: &str) -> Result<Incoming, ReadError> {
    let Value::Object(mut msg) = serde_json::from_str(frame)? else {
        return Err(ReadError::Shape("not an object".to_owned()));
    };
    let id = msg.remove("id").map(serde_json::from_value::<RequestId>).transpose()?;
    let method = match msg.remove("method") {
        Some(Value::String(method)) => Some(method),
        Some(other) => return Err(ReadError::Shape(format!("method {other}"))),
        None => None,
    };
    let params = msg.remove("params").unwrap_or(Value::Null);
    let thread = params.get("threadId").and_then(Value::as_str).map(str::to_owned);
    match (id, method) {
        (Some(id), Some(method)) => Ok(match ServerRequest::read(&method, params) {
            Some(request) => Incoming::Request { id, thread, request: Box::new(request?) },
            None => Incoming::UnknownRequest { id, method },
        }),
        (None, Some(method)) => Ok(match ServerNotification::read(&method, params) {
            Some(note) => Incoming::Notification { thread, note: Box::new(note?) },
            None => Incoming::UnknownNotification { method },
        }),
        (Some(id), None) => Ok(Incoming::Answer { id, outcome: outcome(&mut msg)? }),
        (None, None) => Err(ReadError::Shape("neither a method nor an id".to_owned())),
    }
}

fn outcome(msg: &mut Map<String, Value>) -> Result<Result<Value, RpcError>, ReadError> {
    if let Some(result) = msg.remove("result") {
        return Ok(Ok(result));
    }
    let Some(Value::Object(mut error)) = msg.remove("error") else {
        return Err(ReadError::Shape("an answer with neither a result nor an error".to_owned()));
    };
    let code = error.get("code").and_then(Value::as_i64).unwrap_or_default();
    let message = match error.remove("message") {
        Some(Value::String(message)) => message,
        _ => String::new(),
    };
    Ok(Err(RpcError { code, message, data: error.remove("data") }))
}

/// Request `params` of its method, as request `id`.
///
/// # Errors
///
/// When `params` do not serialise, which a generated type always does.
pub fn request<M: Method>(id: &RequestId, params: &M) -> Result<String, serde_json::Error> {
    serde_json::to_string(&json!({ "id": id, "method": M::METHOD, "params": params }))
}

/// Notification `method`, which carries nothing (`initialized`).
#[must_use]
pub fn notification(method: &str) -> String {
    json!({ "method": method }).to_string()
}

/// The answer `result` to the app-server's request `id`.
///
/// # Errors
///
/// When `result` does not serialise, which a generated type always does.
pub fn answer<T: Serialize>(id: &RequestId, result: &T) -> Result<String, serde_json::Error> {
    serde_json::to_string(&json!({ "id": id, "result": result }))
}

/// Refuse the app-server's request `id` with `code` and `message`.
#[must_use]
pub fn refuse(id: &RequestId, code: i64, message: &str) -> String {
    json!({ "id": id, "error": { "code": code, "message": message } }).to_string()
}

/// What request `M` answered, read from its result.
///
/// # Errors
///
/// When the result is not what `M` answers with.
pub fn response<M>(result: Value) -> Result<M::Response, serde_json::Error>
where
    M: Method,
    M::Response: DeserializeOwned,
{
    serde_json::from_value(result)
}
