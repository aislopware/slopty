// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! slopty-view-doc — session & workspace Loro doc schemas and the typed mirror layer.
//!
//! Port of slopty's `packages/session-doc`. The schema SHAPE (container names, part maps with
//! LoroText bodies, command entries) is kept identical to the TS implementation so the edge's
//! tail materializer and any TS peer remain compatible.
//!
//! Load-bearing invariant (measured in slopty, `oplog-shape.test.ts`): message parts are a
//! LoroList of part maps whose text bodies live in **LoroText** — streaming appends RLE-merge at
//! ~1.03x oplog overhead, whereas rewriting whole part values costs ~125x.
//!
//! slopty: only the transcript, part, command and queue types the interface reads are kept; the
//! Loro documents, the registry, rebuild, workspace and voice docs are cut (`vendor/slopty/README.md`).

// slopty: Slopty's own shapes: the calls beyond zeron's, the dock, the tray, a page's card, a
// part's facts and the permission rules.
pub mod calls;
pub mod commands;
pub mod constants;
pub mod dock;
pub mod page_card;
pub mod part_kinds;
pub mod parts;
pub mod queue;
pub mod rules;
pub mod schema;
pub mod transcript_delta;
pub mod work;

pub use commands::*;
pub use constants::*;
pub use dock::*;
pub use page_card::*;
pub use part_kinds::*;
pub use parts::*;
pub use queue::*;
pub use rules::*;
pub use schema::*;
pub use transcript_delta::*;
pub use work::*;
