// Derived from zeron (MIT, github.com/zeronsh/zeron @ 46d4e42), see THIRD_PARTY_NOTICES.md
//! Block-level markdown over pulldown-cmark, shared by every frontend.
//!
//! - [`parser`] builds a [`BlockTree`] of top-level blocks with source ranges;
//!   [`IncrementalParser`] reparses only from the last stable top-level block boundary, so a
//!   streamed delta costs O(delta + last block).
//! - [`mend`] auto-closes hanging inline markers in the streaming *display* parse only; the
//!   canonical tree settles honestly on completion.

#![forbid(unsafe_code)]
#![warn(unreachable_pub)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "`unreachable_pub` is on, so an item shared from a private module is `pub(crate)`"
)]

pub mod mend;
pub mod parser;

pub use parser::{
    Block, BlockTree, IncrementalParser, InlineRun, InlineStyle, TopBlock, parse_full,
    parse_message,
};
