// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Block-level markdown over pulldown-cmark, shared by every frontend.
//!
//! - [`parser`] builds a [`BlockTree`] of top-level blocks with source ranges;
//!   [`IncrementalParser`] reparses only from the last stable top-level block
//!   boundary, so a streamed delta costs O(delta + last block).
//! - [`mend`] auto-closes hanging inline markers in the streaming *display*
//!   parse only; the canonical tree settles honestly on completion.

pub mod mend;
pub mod parser;

// slopty: `Math`, the TeX runs.
pub use parser::{
    Block, BlockTree, IncrementalParser, InlineRun, InlineStyle, Math, TopBlock, parse_full,
};
