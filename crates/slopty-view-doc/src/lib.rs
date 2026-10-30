// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! slopty-view-doc — the shapes the view model serves the shell beyond the wire types
//! (`slopty_view_proto`): a transcript's entries and parts ([`schema`], [`parts`],
//! [`part_kinds`]) and the frames its watch sends ([`transcript_delta`]); what a session says
//! beside its terminal ([`dock`], [`work`]) and the pages it shows ([`page_card`]); the calls
//! that serve them ([`calls`]); and the size past which an entry splits ([`constants`]).
pub mod calls;
pub mod constants;
pub mod dock;
pub mod page_card;
pub mod part_kinds;
pub mod parts;
pub mod schema;
pub mod transcript_delta;
pub mod work;

pub use constants::*;
pub use dock::*;
pub use page_card::*;
pub use part_kinds::*;
pub use parts::*;
pub use schema::*;
pub use transcript_delta::*;
pub use work::*;
