//! A page an agent shows inline (Slopty's own `show_page`, an MCP App's UI, an `.html` or `.svg`
//! file), as the transcript's page card reads it.
//!
//! The card is the shell's (`slopty_shell::page`); the web view in it is the app's, handed over by
//! the page key [`PageCard::page`] through `slopty_shell::live`, as a terminal's is. What the page
//! asks of the person (a tool call to confirm, network to allow) and how it stands come on
//! `slopty.WatchPage` ([`crate::calls::WATCH_PAGE`]) as a [`PageState`]; the person's answers go by
//! `slopty.AnswerPage` and `slopty.GrantPageNetwork`. A page never answers for the person: a
//! message it sends becomes words in the chat's draft, context it offers goes there too, visibly,
//! and a tool call it makes waits for the person's own Allow
//! (`docs/decisions/ui.md`, "A page never answers for the person").

use serde::{Deserialize, Serialize};

/// A page shown by a call, as its card reads it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageCard {
    /// The card's part id: `{call part id}:page`.
    pub id: String,
    /// The page's key, which the app hosts its view by and `slopty.WatchPage` names:
    /// `slopty-page:{thread}/{item}`.
    pub page: String,
    /// Its name: the header's first words and what assistive technologies read.
    pub title: String,
    /// What it is: [`PageCard::HTML`], [`PageCard::FILE`], [`PageCard::APP`].
    pub kind: String,
    /// Where it comes from, as the header's last words say it: the file's path (a link to the
    /// Files pane), `{server} · {tool}` for an MCP App, `show_page` for HTML the agent wrote.
    pub source: String,
    /// For a file: its path on the chat's machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// For an MCP App: its server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// For an MCP App: the tool called.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// For an MCP App: its UI resource (`ui://…`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_uri: Option<String>,
    /// How the page prefers to be shown (`inline`, `fullscreen`, `pip`), when it says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_mode: Option<String>,
    /// The call's input still streams: the page shows what has come.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub streaming: bool,
    /// The call failed or was stopped: the card says so, and the page is told.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub failed: bool,
}

impl PageCard {
    /// An MCP App's UI, read through the agent.
    pub const APP: &'static str = "app";
    /// A file on the chat's machine (`.html`, `.svg`).
    pub const FILE: &'static str = "file";
    /// HTML the agent wrote into the call.
    pub const HTML: &'static str = "html";
}

/// How a page stands, and what it asks of the person.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageState {
    /// The page's key.
    pub page: String,
    /// How it is shown now: [`PageState::COLD`], [`PageState::WARM`], [`PageState::LIVE`]; the
    /// card's assistive value.
    pub presence: String,
    /// Its height in whole points, as the page asked within the inline cap, once it said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// Its document has loaded.
    pub loaded: bool,
    /// Why it could not be shown, when it could not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<String>,
    /// A tool call it asks to make, waiting for the person's Allow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirm: Option<PageConfirm>,
    /// The origins its app declares it needs, while not granted: "Wants network: a.example ·
    /// Allow".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network: Option<PageNetwork>,
    /// Where the page shows: `inline`, or `pane` once the person moved it to the right pane
    /// (`slopty.MovePage`).
    pub display_mode: String,
    /// Tool calls it made, each with the person's Allow: the card's "Page called {tool}" lines.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub called: Vec<String>,
}

impl PageState {
    /// No web view: its place, or its last picture, stands for it.
    pub const COLD: &'static str = "cold";
    /// Placed, taking the pointer and the keyboard.
    pub const LIVE: &'static str = "live";
    /// A web view not placed: its snapshot stands for it.
    pub const WARM: &'static str = "warm";
}

/// A tool call a page asks to make.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageConfirm {
    /// The page's request, which `slopty.AnswerPage` answers.
    pub request: String,
    /// The tool.
    pub tool: String,
    /// Its arguments, as JSON, cut to a few lines.
    pub arguments: String,
}

/// The network a page's app declares, not yet granted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PageNetwork {
    /// What it would fetch and open sockets to.
    pub connect: Vec<String>,
    /// What it would load scripts, styles, images, fonts and media from.
    pub resources: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A card keeps only what it has on the wire, camel-cased.
    #[test]
    fn a_card_travels_lean() {
        let card = PageCard {
            id: "t1:page".into(),
            page: "slopty-page:th/t1".into(),
            title: "Latency".into(),
            kind: PageCard::HTML.into(),
            source: "show_page".into(),
            ..PageCard::default()
        };
        let json = serde_json::to_value(&card).expect("serializes");
        assert!(json.get("resourceUri").is_none());
        assert!(json.get("streaming").is_none());
        assert_eq!(json["page"], "slopty-page:th/t1");
    }
}
