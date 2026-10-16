//! The MCP Apps bridge as the host hears it: JSON-RPC 2.0 from the view, relayed by the host
//! document's script, read against an allowlist.
//!
//! The host answers the handshake (`ui/initialize`) and `ping` itself, and reads the view's size
//! and logs. Everything that would act for the person (a tool call, a message, context for the
//! model, a link, a display mode, a resource read) goes to the app as an [`Ask`], unanswered: the
//! app answers once the person has decided. Any other method is refused with JSON-RPC's
//! "method not found", and counted (`ext-apps` `specification/2026-01-26/apps.mdx`).

use serde_json::{Value, json};

use super::{Ask, Device, Theme, sandbox};

/// The version of MCP Apps this host speaks.
pub(crate) const PROTOCOL: &str = "2026-01-26";

/// The longest message the relay passes, in UTF-16 units of its JSON: a tool call's arguments
/// fit many times over.
pub(crate) const MAX_MESSAGE: usize = 4 << 20;

/// What the host document's script relayed.
#[derive(Debug, PartialEq)]
pub(crate) enum Relayed {
    /// A message the view posted to its parent, and whether the person had just acted in the
    /// page (`navigator.userActivation.isActive`).
    Message { message: Value, activated: bool },
    /// The view's document finished loading.
    ViewLoaded,
}

/// Read the relay's envelope: `{"k":"m","m":…,"a":…}` or `{"k":"load"}`.
pub(crate) fn relayed(text: &str) -> Option<Relayed> {
    if text.len() > MAX_MESSAGE.saturating_mul(3) {
        return None;
    }
    let envelope: Value = serde_json::from_str(text).ok()?;
    match envelope.get("k")?.as_str()? {
        "m" => Some(Relayed::Message {
            message: envelope.get("m")?.clone(),
            activated: envelope.get("a").and_then(Value::as_bool).unwrap_or(false),
        }),
        "load" => Some(Relayed::ViewLoaded),
        _ => None,
    }
}

/// What the host does with one message from the view.
#[derive(Debug, PartialEq)]
pub(crate) enum Route {
    /// `ui/initialize`: answered at once with the host's context.
    Initialize { id: Value },
    /// `ping`: answered at once.
    Ping { id: Value },
    /// `ui/notifications/initialized`: the host may now send the view its notices.
    Initialized,
    /// `ui/notifications/size-changed`, in CSS pixels.
    Size { height: f64 },
    /// `notifications/message`: the view's log.
    Log { level: String, logger: Option<String>, data: Value },
    /// A request for the person: the app answers it, or no one does.
    Ask { id: Value, ask: Ask },
    /// The view's answer to a request of the host's (`ui/resource-teardown`).
    Answered { id: Value },
    /// A request whose parameters do not fit its method: answered with "invalid params".
    Invalid { id: Value, method: String },
    /// A method off the allowlist; a request (with its id) is answered "method not found".
    Refused { id: Option<Value>, method: String },
    /// Not JSON-RPC 2.0.
    Malformed,
}

/// Where `message` goes.
pub(crate) fn route(message: &Value) -> Route {
    if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Route::Malformed;
    }
    let id = message.get("id").filter(|id| id.is_string() || id.is_number()).cloned();
    let Some(method) = message.get("method").and_then(Value::as_str) else {
        let answered = message.get("result").is_some() || message.get("error").is_some();
        return match id {
            Some(id) if answered => Route::Answered { id },
            _ => Route::Malformed,
        };
    };
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let text = |key: &str| params.get(key).and_then(Value::as_str).map(str::to_owned);
    let Some(id) = id else {
        return match method {
            "ui/notifications/initialized" => Route::Initialized,
            "ui/notifications/size-changed" => match params.get("height").and_then(Value::as_f64) {
                Some(height) => Route::Size { height },
                None => Route::Malformed,
            },
            "notifications/message" => Route::Log {
                level: text("level").unwrap_or_else(|| "info".to_owned()),
                logger: text("logger"),
                data: params.get("data").cloned().unwrap_or(Value::Null),
            },
            _ => Route::Refused { id: None, method: method.to_owned() },
        };
    };
    let ask = match method {
        "ui/initialize" => return Route::Initialize { id },
        "ping" => return Route::Ping { id },
        "tools/call" => text("name").map(|name| Ask::CallTool {
            name,
            arguments: params.get("arguments").cloned().unwrap_or_else(|| json!({})),
        }),
        "resources/read" => text("uri").map(|uri| Ask::ReadResource { uri }),
        "ui/message" => params.get("content").map(|content| Ask::Message {
            role: text("role").unwrap_or_else(|| "user".to_owned()),
            content: content.clone(),
        }),
        "ui/update-model-context" => {
            params.is_object().then(|| Ask::UpdateContext { params: params.clone() })
        }
        "ui/open-link" => text("url").map(|url| Ask::OpenLink { url }),
        "ui/request-display-mode" => text("mode").map(|mode| Ask::DisplayMode { mode }),
        _ => return Route::Refused { id: Some(id), method: method.to_owned() },
    };
    match ask {
        Some(ask) => Route::Ask { id, ask },
        None => Route::Invalid { id, method: method.to_owned() },
    }
}

/// The host's side of a page, as `hostContext` tells the view.
#[derive(Debug)]
pub(crate) struct Context<'a> {
    theme: &'a Theme,
    device: Device,
    display_mode: &'a str,
    width: f64,
    max_height: f64,
    locale: &'a str,
}

impl<'a> Context<'a> {
    /// A page `width` points wide, at most `max_height` high, shown as `display_mode`.
    pub(crate) const fn new(
        theme: &'a Theme,
        device: Device,
        display_mode: &'a str,
        width: f64,
        max_height: f64,
        locale: &'a str,
    ) -> Self {
        Self { theme, device, display_mode, width, max_height, locale }
    }

    /// `hostContext`.
    pub(crate) fn json(&self) -> Value {
        let variables: serde_json::Map<String, Value> = sandbox::variables(self.theme)
            .map(|(name, value)| (name.to_owned(), Value::String(value.to_owned())))
            .collect();
        let (touch, hover) = self.device.touch_and_hover();
        json!({
            "theme": if self.theme.dark { "dark" } else { "light" },
            "styles": { "variables": variables },
            "displayMode": self.display_mode,
            "availableDisplayModes": ["inline", "fullscreen"],
            "containerDimensions": { "width": self.width, "maxHeight": self.max_height },
            "locale": self.locale,
            "platform": self.device.platform(),
            "deviceCapabilities": { "touch": touch, "hover": hover },
        })
    }

    /// What the host answers `ui/initialize` with: its version, what it can do for the view
    /// (through the person, for the app to decide), and its context.
    pub(crate) fn initialized(&self, id: &Value, server: bool, granted: &sandbox::Grants) -> Value {
        let sources = |origins: &[sandbox::Origin]| -> Vec<String> {
            origins.iter().map(sandbox::Origin::to_string).collect()
        };
        let mut capabilities = json!({
            "openLinks": {},
            "logging": {},
            "sandbox": {
                "permissions": {},
                "csp": {
                    "connectDomains": sources(granted.connect()),
                    "resourceDomains": sources(granted.resources()),
                },
            },
        });
        if server && let Some(all) = capabilities.as_object_mut() {
            all.insert("serverTools".to_owned(), json!({}));
            all.insert("serverResources".to_owned(), json!({}));
        }
        result(
            id,
            &json!({
                "protocolVersion": PROTOCOL,
                "hostInfo": { "name": "Slopty", "version": env!("CARGO_PKG_VERSION") },
                "hostCapabilities": capabilities,
                "hostContext": self.json(),
            }),
        )
    }
}

/// A JSON-RPC result.
pub(crate) fn result(id: &Value, result: &Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// A JSON-RPC error.
pub(crate) fn error(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// A JSON-RPC notification.
pub(crate) fn notification(method: &str, params: &Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

/// A JSON-RPC request of the host's.
pub(crate) fn request(id: &Value, method: &str, params: &Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

/// JSON-RPC's "method not found".
pub(crate) const METHOD_NOT_FOUND: i64 = -32601;
/// JSON-RPC's "invalid params".
pub(crate) const INVALID_PARAMS: i64 = -32602;

#[cfg(test)]
mod tests {
    use super::*;

    fn rpc(value: &Value) -> Route {
        route(value)
    }

    /// The handshake and `ping` are the host's own; the view's size and logs are read.
    #[test]
    fn the_host_answers_the_handshake_and_reads_size_and_logs() {
        let init = json!({"jsonrpc":"2.0","id":1,"method":"ui/initialize","params":{}});
        assert_eq!(rpc(&init), Route::Initialize { id: json!(1) });
        assert_eq!(
            rpc(&json!({"jsonrpc":"2.0","id":"p","method":"ping"})),
            Route::Ping { id: json!("p") }
        );
        assert_eq!(
            rpc(&json!({"jsonrpc":"2.0","method":"ui/notifications/initialized"})),
            Route::Initialized
        );
        assert_eq!(
            rpc(
                &json!({"jsonrpc":"2.0","method":"ui/notifications/size-changed","params":{"width":3,"height":5000}})
            ),
            Route::Size { height: 5000.0 }
        );
        assert_eq!(
            rpc(
                &json!({"jsonrpc":"2.0","method":"notifications/message","params":{"level":"debug","data":{"a":1}}})
            ),
            Route::Log { level: "debug".to_owned(), logger: None, data: json!({"a":1}) }
        );
    }

    /// What would act for the person goes to the app; a request off the allowlist is refused,
    /// as is a malformed one.
    #[test]
    fn asks_go_to_the_app_and_the_rest_is_refused() {
        let call = json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"echo","arguments":{"v":2}}});
        assert_eq!(
            rpc(&call),
            Route::Ask {
                id: json!(7),
                ask: Ask::CallTool { name: "echo".to_owned(), arguments: json!({"v":2}) }
            }
        );
        let message = json!({"jsonrpc":"2.0","id":8,"method":"ui/message","params":{"role":"user","content":[{"type":"text","text":"hi"}]}});
        assert!(matches!(rpc(&message), Route::Ask { ask: Ask::Message { .. }, .. }));
        let context = json!({"jsonrpc":"2.0","id":9,"method":"ui/update-model-context","params":{"content":[]}});
        assert!(matches!(rpc(&context), Route::Ask { ask: Ask::UpdateContext { .. }, .. }));
        let link = json!({"jsonrpc":"2.0","id":10,"method":"ui/open-link","params":{"url":"https://a.example"}});
        assert!(matches!(rpc(&link), Route::Ask { ask: Ask::OpenLink { .. }, .. }));
        assert_eq!(
            rpc(&json!({"jsonrpc":"2.0","id":11,"method":"tools/call","params":{}})),
            Route::Invalid { id: json!(11), method: "tools/call".to_owned() }
        );
        assert_eq!(
            rpc(&json!({"jsonrpc":"2.0","id":12,"method":"sampling/createMessage","params":{}})),
            Route::Refused { id: Some(json!(12)), method: "sampling/createMessage".to_owned() }
        );
        assert_eq!(
            rpc(&json!({"jsonrpc":"2.0","method":"ui/notifications/sandbox-proxy-ready"})),
            Route::Refused { id: None, method: "ui/notifications/sandbox-proxy-ready".to_owned() }
        );
        assert_eq!(rpc(&json!({"id":1,"method":"ping"})), Route::Malformed);
        assert_eq!(
            rpc(&json!({"jsonrpc":"2.0","id":{"x":1},"method":"ping"})),
            Route::Refused { id: None, method: "ping".to_owned() }
        );
        assert_eq!(
            rpc(&json!({"jsonrpc":"2.0","id":"t","result":{}})),
            Route::Answered { id: json!("t") }
        );
    }

    /// The relay's envelope is read, and anything else dropped.
    #[test]
    fn the_relays_envelope_is_read() {
        assert_eq!(relayed(r#"{"k":"load"}"#), Some(Relayed::ViewLoaded));
        assert_eq!(
            relayed(r#"{"k":"m","m":{"a":1},"a":true}"#),
            Some(Relayed::Message { message: json!({"a":1}), activated: true })
        );
        assert_eq!(relayed(r#"{"k":"x"}"#), None);
        assert_eq!(relayed("not json"), None);
    }

    /// `ui/initialize` gets the theme, the display mode and the size cap; tools and resources are
    /// offered only to an MCP App's page.
    #[test]
    fn the_handshake_carries_the_hosts_context() {
        let theme = Theme {
            dark: true,
            variables: vec![(sandbox::BACKGROUND.to_owned(), "#101418".to_owned())],
        };
        let context = Context::new(&theme, Device::Phone, "inline", 390.0, 600.0, "en-GB");
        let answer = context.initialized(&json!(1), false, &sandbox::Grants::default());
        let result = &answer["result"];
        assert_eq!(result["protocolVersion"], PROTOCOL);
        assert_eq!(result["hostContext"]["theme"], "dark");
        assert_eq!(result["hostContext"]["displayMode"], "inline");
        assert_eq!(result["hostContext"]["platform"], "mobile");
        assert_eq!(result["hostContext"]["containerDimensions"]["maxHeight"], 600.0);
        assert_eq!(result["hostContext"]["styles"]["variables"][sandbox::BACKGROUND], "#101418");
        assert!(result["hostCapabilities"].get("serverTools").is_none());
        let served = context.initialized(&json!(1), true, &sandbox::Grants::default());
        assert!(served["result"]["hostCapabilities"].get("serverTools").is_some());
    }
}
