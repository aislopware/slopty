//! Claude Code's hook payloads, as the relay hands them to the worker
//! (`slopty_agent::hooks`).
//!
//! Each line of the input is one payload, as `slopty hook` reads it from Claude Code's stdin:
//! JSON whose every field (a tool's input, a notification's words, a prompt) carries what the
//! model, the person or a tool wrote. What must hold, beside no panic: a payload reads the same
//! with a field no Claude Code writes beside its own (the decoder passes over what it does not
//! know), and what it says of the session is in a line: a dialog's words name what it asks on
//! one line, as the sidebar shows it.

use serde_json::Value;
use slopty_agent::hooks::{Hook, Signal};

/// The field no Claude Code writes, put in every payload for the unknown-field relation.
const UNKNOWN: &str = "slopty_fuzz_unknown";

/// Run one input.
pub fn run(data: &[u8]) {
    let text = String::from_utf8_lossy(data);
    for line in text.lines() {
        payload(line);
    }
}

/// One payload.
fn payload(line: &str) {
    let Some(hook) = Hook::parse(line) else { return };
    let signal = hook.signal();
    if let Signal::Asks { kind, text } = &signal {
        assert!(!kind.is_empty(), "a dialog of no kind");
        assert!(!text.contains(['\n', '\r']), "a dialog's words are not one line: {text:?}");
    }
    if let Ok(Value::Object(mut fields)) = serde_json::from_str::<Value>(line)
        && !fields.contains_key(UNKNOWN)
    {
        fields.insert(UNKNOWN.to_owned(), Value::Bool(true));
        let again = Hook::parse(&Value::Object(fields).to_string());
        assert_eq!(again.as_ref(), Some(&hook), "a field no Claude Code writes changed the hook");
        assert_eq!(again.map(|h| h.signal()), Some(signal), "and what it says");
    }
}
