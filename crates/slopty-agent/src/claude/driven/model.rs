//! The model's settings as Claude Code's TUI says them (`.research/tui-strings-2026-10-16.md`
//! §4.Mode and §4.Model): the permission mode and the mark a change the person did not make
//! leaves in the transcript ("Switched to accept edits"), fast mode as it stands (on, off, or
//! cooling down after its limit) with the TUI's notes when it moves, whether ultracode is on, and
//! thinking turned on or off.
//!
//! - **Mode.** Every report of the mode goes through [`Driven::mode_now`]. A change Claude Code
//!   made itself (a plan approved with a mode, plan mode entered or left) is a quiet transcript
//!   row; the person's own switch is not, since the footer already says it. The mode plan mode was
//!   entered from is kept: a plan approved with the context cleared goes on in it. Plan mode in a
//!   session that began in bypass permissions says it will ask before almost nothing.
//! - **Fast mode.** `fast_mode_state` (init, every result) is the thread's fact
//!   ([`super::FAST_FACT`]); a limit that cools it down, its reset, and fast mode asked for and not
//!   served are passing notes in the TUI's words.
//! - **Ultracode.** `get_settings` says whether it applies (`applied.ultracode`) and whether the
//!   session can have it (`ultracodeAvailable`), asked after `initialize` and after the model or
//!   the effort changes ([`super::ULTRACODE_FACT`]). A settings file it skipped for its errors is
//!   said once.

use serde_json::{Value, json};
use slopty_core::WallMs;
use slopty_proto::thread::{Action, AgentNote, Notice};

use super::{Control, Driven, FAST_FACT, ULTRACODE_FACT, bool_at, str_at};
use crate::claude::history::Out;

/// The fact that keeps whether thinking is on, once the person turned it on or off.
pub const THINKING_FACT: &str = "claude-thinking";

/// What plan mode says in a session that began in bypass permissions, as the TUI says it.
const BYPASS_PLANS: &str = "Session was started with bypass permissions, so plan mode won't ask \
     before most edits or commands. Start a new session without bypass permissions for prompts \
     in plan mode.";

/// How long a passing note about the model shows, as the TUI's notifications last.
const NOTE_FOR_MS: u64 = 8_000;

/// A permission mode in the words its footer chip uses: `acceptEdits` reads "accept edits",
/// `plan` "plan mode", `default` "manual mode".
#[must_use]
pub(super) fn mode_words(mode: &str) -> String {
    match mode {
        "acceptEdits" => "accept edits".to_owned(),
        "bypassPermissions" => "bypass permissions".to_owned(),
        "dontAsk" => "don't ask".to_owned(),
        "plan" => "plan mode".to_owned(),
        "auto" => "auto mode".to_owned(),
        "default" | "manual" => "manual mode".to_owned(),
        other => other.to_owned(),
    }
}

/// The transcript's mark of a switch to `mode`.
#[must_use]
pub(super) fn switched_to(mode: &str) -> String {
    format!("Switched to {}", mode_words(mode))
}

/// Why fast mode cannot serve, in Claude Code's own words for its reason.
#[must_use]
pub(super) fn fast_unavailable(reason: &str) -> String {
    match reason {
        "not_first_party" => "Fast mode is only available when using the Anthropic API directly",
        "disabled_by_env" => "Fast mode is not available",
        "model_not_allowed" => "The fast model is not in your organization's allowed models",
        "sdk_opt_in_required" => "Fast mode is not available in the Agent SDK",
        "pending" => "Checking fast mode availability",
        "free" => "Fast mode requires a paid subscription",
        "preference" => "Fast mode has been disabled by your organization",
        "extra_usage_disabled" => "Fast mode needs usage credits turned on",
        "network_error" => "Fast mode unavailable due to network connectivity issues",
        _ => "Fast mode is currently unavailable",
    }
    .to_owned()
}

impl Driven {
    /// The permission mode is `mode` now. `marked` when Claude Code changed it itself (not the
    /// person's switch): the transcript says so.
    pub(super) fn mode_now(&mut self, mode: String, marked: bool, now: WallMs) -> Vec<Out> {
        let had = self.history.meters_now().mode.clone();
        if had.is_none() && mode == "bypassPermissions" {
            self.started_bypass = true;
        }
        let planning = mode == "plan" && had.as_deref() != Some("plan");
        if planning {
            self.before_plan.clone_from(&had);
        }
        let asked = self.controls.values().any(|c| matches!(c, Control::SetMode(_)));
        let changed = had.as_deref().is_some_and(|had| had != mode);
        let mut outs = Vec::new();
        if marked && changed && !asked {
            outs.extend(self.history.notice(Notice::MODE, &switched_to(&mode), now));
        }
        if planning && had.is_some() && self.started_bypass {
            outs.extend(self.history.notice(Notice::MODE, BYPASS_PLANS, now));
        }
        outs.extend(self.meters(|m| m.mode = Some(mode)));
        outs
    }

    /// The mode plan mode was entered from, when it was.
    pub(super) fn before_plan(&self) -> Option<String> {
        self.before_plan.clone()
    }

    /// A passing note about the model, keyed `key` so a newer one replaces it.
    pub(super) fn passing(&mut self, key: &str, text: &str, now: WallMs) -> Vec<Out> {
        let note = AgentNote {
            key: format!("model:{key}"),
            text: text.to_owned(),
            priority: "medium".to_owned(),
            color: None,
            at_ms: now,
            until_ms: Some(WallMs::from_millis(now.as_millis().saturating_add(NOTE_FOR_MS))),
        };
        self.history.tell(Action::Notified(note))
    }

    /// Fast mode stands at `state` (`on`, `off`, `cooldown`), with `reason` why it cannot serve
    /// when Claude Code says: the fact, and the TUI's note when it moved.
    pub(super) fn fast_now(&mut self, state: &str, reason: Option<&str>, now: WallMs) -> Vec<Out> {
        let had = self.fast.0.replace(state.to_owned());
        let asked = self.fast.1;
        let mut outs = self.history.fact(FAST_FACT, Some(state));
        let note = match (had.as_deref(), state) {
            (Some("on"), "cooldown") => Some("Fast limit reached and temporarily disabled"),
            (Some("cooldown"), "on") => Some("Fast limit reset · now using fast mode"),
            (Some("cooldown"), "off") if asked => Some("Fast mode is now available"),
            _ => None,
        };
        if let Some(note) = note {
            outs.extend(self.passing("fast", note, now));
        } else if asked && state == "off" && had.as_deref() != Some("off") {
            // Asked for and not served: why, as the TUI says it.
            let why =
                reason.map_or_else(|| "fast mode wasn't available".to_owned(), fast_unavailable);
            outs.extend(self.passing("fast", &format!("Using standard speed · {why}"), now));
        }
        outs
    }

    /// What a result or `initialize` says of fast mode.
    pub(super) fn fast_said(&mut self, said: &Value, now: WallMs) -> Vec<Out> {
        let Some(state) = str_at(said, "fast_mode_state") else { return Vec::new() };
        let state = match state {
            "on" | "cooldown" => state.to_owned(),
            _ => "off".to_owned(),
        };
        let reason = str_at(said, "fast_mode_disabled_reason").map(str::to_owned);
        if self.fast.0.as_deref() == Some(state.as_str()) {
            return Vec::new();
        }
        self.fast_now(&state, reason.as_deref(), now)
    }

    /// Ask how the settings apply, for ultracode.
    pub(super) fn ask_settings(&mut self) {
        let id = self.control(Control::Settings, &json!({ "subtype": "get_settings" }));
        self.quiet.insert(id);
    }

    /// What `get_settings` said, at `now`: ultracode on or off where the session can have it,
    /// and a settings file Claude Code skipped for its errors, said once.
    pub(super) fn settings_read(&mut self, said: &Value, now: WallMs) -> Vec<Out> {
        let mut outs = Vec::new();
        let errors: Vec<&Value> =
            said.get("errors").and_then(Value::as_array).into_iter().flatten().collect();
        if let Some(first) = errors.first() {
            let file = str_at(first, "file").unwrap_or("A settings file");
            let message = str_at(first, "message").unwrap_or("it does not read");
            let more = errors.len().saturating_sub(1);
            let words = if more > 0 {
                format!("{file} was skipped: {message} (and {more} more)")
            } else {
                format!("{file} was skipped: {message}")
            };
            if self.said_once.0.insert(format!("settings:{words}")) {
                outs.extend(self.passing("settings", &words, now));
            }
        }
        let Some(applied) = said.get("applied") else { return outs };
        // The advisor rides every request: the TUI says so once, as it costs tokens.
        if let Some(advisor) = str_at(applied, "advisor").filter(|a| !a.trim().is_empty())
            && self.said_once.0.insert(format!("advisor:{advisor}"))
        {
            let words = format!(
                "Advisor Tool (experimental) is on and may use more tokens · advising with {}",
                crate::claude::model_label(advisor)
            );
            outs.extend(self.passing("advisor", &words, now));
        }
        let on = bool_at(applied, "ultracode");
        let available = bool_at(applied, "ultracodeAvailable") || on;
        let value = available.then_some(if on { "on" } else { "off" });
        outs.extend(self.history.fact(ULTRACODE_FACT, value));
        outs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Modes read as the footer says them; every reason fast mode is off has its words.
    #[test]
    fn modes_and_fast_reasons_read_as_the_tui_says_them() {
        assert_eq!(switched_to("acceptEdits"), "Switched to accept edits");
        assert_eq!(switched_to("default"), "Switched to manual mode");
        assert_eq!(mode_words("somethingNew"), "somethingNew");
        assert_eq!(fast_unavailable("free"), "Fast mode requires a paid subscription");
        assert_eq!(fast_unavailable("whatever"), "Fast mode is currently unavailable");
    }
}
