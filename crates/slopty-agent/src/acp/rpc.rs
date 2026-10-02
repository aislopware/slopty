//! ACP's framing: JSON-RPC 2.0, one message a line on the agent's stdin and stdout.
//!
//! The transport says a message is ended by LF and holds none of its own ("MUST NOT contain
//! embedded newlines"), so a line is a message. Envelopes and payloads are the protocol's own
//! types (`agent_client_protocol_schema`); what comes in is read as JSON first and classed by
//! its members, so a message of a method this does not know is still answered or passed over,
//! never a parse error that stalls the agent.

use std::sync::Arc;

use agent_client_protocol_schema::v1::{self as acp, JsonRpcMessage, Notification, RequestId};
use serde::Serialize;
use serde_json::Value;

/// A message from the agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Incoming {
    /// The answer to one of ours.
    Response {
        /// Ours, as we numbered it.
        id: RequestId,
        /// What it came to.
        outcome: Result<Value, acp::Error>,
    },
    /// A request of the agent's: it waits on the answer.
    Request {
        /// Its id, which the answer repeats.
        id: RequestId,
        /// What it asks.
        method: String,
        /// With what.
        params: Value,
    },
    /// A notification: nothing answers it.
    Notification {
        /// What it says.
        method: String,
        /// With what.
        params: Value,
    },
}

/// The message in `line`, its LF (and a CR before it) stripped.
///
/// # Errors
///
/// When the line is not a JSON-RPC message.
pub fn incoming(line: &[u8]) -> Result<Incoming, String> {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let mut message: serde_json::Map<String, Value> =
        serde_json::from_slice(line).map_err(|e| e.to_string())?;
    let id = message
        .remove("id")
        .map(serde_json::from_value::<RequestId>)
        .transpose()
        .map_err(|e| format!("an id that is none: {e}"))?;
    let params = message.remove("params").unwrap_or(Value::Null);
    match (message.remove("method"), id) {
        (Some(Value::String(method)), Some(id)) => Ok(Incoming::Request { id, method, params }),
        (Some(Value::String(method)), None) => Ok(Incoming::Notification { method, params }),
        (Some(_), _) => Err("a method that is not a string".to_owned()),
        (None, Some(id)) => {
            let outcome = match (message.remove("result"), message.remove("error")) {
                (_, Some(error)) => {
                    Err(serde_json::from_value(error).map_err(|e| format!("an error: {e}"))?)
                }
                (result, None) => Ok(result.unwrap_or(Value::Null)),
            };
            Ok(Incoming::Response { id, outcome })
        }
        (None, None) => Err("neither a method nor an id".to_owned()),
    }
}

/// Request `method` with `params`, numbered `id`, as one line.
///
/// # Errors
///
/// When `params` does not serialize.
pub fn request(id: u64, method: &str, params: &impl Serialize) -> serde_json::Result<Vec<u8>> {
    let id = RequestId::Number(i64::try_from(id).unwrap_or(i64::MAX));
    line(&JsonRpcMessage::wrap(acp::Request {
        id,
        method: Arc::from(method),
        params: Some(serde_json::to_value(params)?),
    }))
}

/// Notification `method` with `params`, as one line.
///
/// # Errors
///
/// When `params` does not serialize.
pub fn notification(method: &str, params: &impl Serialize) -> serde_json::Result<Vec<u8>> {
    line(&JsonRpcMessage::wrap(Notification {
        method: Arc::from(method),
        params: Some(serde_json::to_value(params)?),
    }))
}

/// The answer `result` to the agent's request `id`, as one line.
///
/// # Errors
///
/// When `result` does not serialize.
pub fn result(id: &RequestId, result: &impl Serialize) -> serde_json::Result<Vec<u8>> {
    let response: acp::Response<Value> =
        acp::Response::new(id.clone(), Ok(serde_json::to_value(result)?));
    line(&JsonRpcMessage::wrap(response))
}

/// The agent's request `id` refused with `error`, as one line.
///
/// # Errors
///
/// Never for these types; serde's signature has it.
pub fn error(id: &RequestId, error: acp::Error) -> serde_json::Result<Vec<u8>> {
    let response: acp::Response<Value> = acp::Response::new(id.clone(), Err(error));
    line(&JsonRpcMessage::wrap(response))
}

fn line(message: &impl Serialize) -> serde_json::Result<Vec<u8>> {
    let mut out = serde_json::to_vec(message)?;
    out.push(b'\n');
    Ok(out)
}

/// An id as the thread names what it belongs to: a number or a string as written.
#[must_use]
pub fn id_text(id: &RequestId) -> String {
    match id {
        RequestId::Null => "null".to_owned(),
        RequestId::Number(n) => n.to_string(),
        RequestId::Str(s) => s.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_classed_by_their_members() {
        let read = |s: &str| incoming(s.as_bytes()).unwrap();
        assert_eq!(
            read("{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"a\":1}}\r\n"),
            Incoming::Response {
                id: RequestId::Number(3),
                outcome: Ok(serde_json::json!({"a": 1}))
            }
        );
        let Incoming::Response { outcome: Err(error), .. } = read(
            "{\"jsonrpc\":\"2.0\",\"id\":\"x\",\"error\":{\"code\":-32000,\"message\":\"sign in\"}}",
        ) else {
            panic!("an error")
        };
        assert_eq!((error.code, error.message.as_str()), (acp::ErrorCode::AuthRequired, "sign in"));
        assert!(matches!(
            read(
                "{\"jsonrpc\":\"2.0\",\"id\":0,\"method\":\"session/request_permission\",\"params\":{}}"
            ),
            Incoming::Request { id: RequestId::Number(0), .. }
        ));
        assert!(matches!(
            read("{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{}}"),
            Incoming::Notification { .. }
        ));
        incoming(b"{\"jsonrpc\":\"2.0\"}").unwrap_err();
        incoming(b"not json").unwrap_err();
    }

    #[test]
    fn what_goes_out_is_one_line_of_json_rpc() {
        let sent = request(7, "session/cancel", &serde_json::json!({"sessionId": "s"})).unwrap();
        assert!(sent.strip_suffix(b"\n").is_some_and(|body| !body.contains(&b'\n')), "one line");
        let value: Value = serde_json::from_slice(&sent).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"jsonrpc": "2.0", "id": 7, "method": "session/cancel", "params": {"sessionId": "s"}})
        );
        let refused =
            error(&RequestId::Str("q".to_owned()), acp::Error::method_not_found()).unwrap();
        let value: Value = serde_json::from_slice(&refused).unwrap();
        assert_eq!(value["error"]["code"], -32601);
        assert_eq!(value["id"], "q");
    }
}
