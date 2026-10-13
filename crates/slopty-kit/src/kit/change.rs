//! Motion keyed on a value changing.
//!
//! [`on_change`] counts how often a value changed since its element first drew, so an
//! animation keyed on the count plays once per change and never on the first paint.
//!
//! Ported from Ely GPUI Components (`src/motion/changes.rs`), Copyright (c) 2026 Ely GPUI
//! Component contributors, MIT OR Apache-2.0.

use gpui::{App, ElementId, Window};

/// What a keyed element last drew, and how often that changed.
struct Changes<T> {
    last: T,
    count: u64,
}

/// How often `value` changed since the element `id` first drew it.
///
/// 0 on the first paint, one more on each change. Key an animation on it so it plays once per
/// change and is still when the element first appears; it never re-keys what is inside, so focus
/// and hover there hold.
pub fn on_change<T: Clone + PartialEq + 'static>(
    id: impl Into<ElementId>,
    value: &T,
    window: &mut Window,
    cx: &mut App,
) -> u64 {
    let state = window.use_keyed_state(id, cx, |_, _| Changes { last: value.clone(), count: 0 });
    if state.read(cx).last != *value {
        state.update(cx, |state, _| {
            state.last = value.clone();
            state.count = state.count.saturating_add(1);
        });
    }
    state.read(cx).count
}
