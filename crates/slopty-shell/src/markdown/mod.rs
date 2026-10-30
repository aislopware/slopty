// Derived from zeron (MIT, github.com/zeronsh/zeron @ 46d4e42), see THIRD_PARTY_NOTICES.md
//! Our own markdown stack — pulldown-cmark parse into a [`parser::BlockTree`],
//! block-level incremental reparse for streaming, gpui rendering, and a
//! Tree-sitter paint-only syntax highlighting. No zed GPL crates.
//!
//! Design (docs/research/mugen-pretext.md §2):
//! - the parse is block-granular and append-incremental: streaming reparses only
//!   from the last stable top-level block boundary;
//! - highlighting is **pure paint** — token colors on identical mono runs, so
//!   layout never depends on it;
//! - streaming fade-in is a per-appended-chunk opacity veil over the text runs
//!   ([`veil`]) — paint-only, never measured, layout commits instantly;
//! - hanging inline markers (`**bold`, `[link](url…`) are auto-closed in the
//!   streaming *display* parse only ([`mend`]), so closing markers never
//!   reflow already-painted text; the canonical parse settles honestly on
//!   completion.

// Find in a thread, the paint half.
pub mod find;
pub(crate) mod inline_code_links;
mod link_destination;
mod link_interaction;
mod link_presentation;
pub mod links;
pub(crate) mod math;
pub use slopty_markdown::{mend, parser};
pub mod render;
pub mod selection;
pub mod veil;

pub use parser::{
    Block, BlockTree, IncrementalParser, InlineRun, InlineStyle, parse_full, parse_message,
};

pub(crate) mod ansi; // terminal output in its colours.
pub(crate) mod chat_links; // links to chats as live chips.
pub mod mermaid;
pub(crate) mod mermaid_cache;
