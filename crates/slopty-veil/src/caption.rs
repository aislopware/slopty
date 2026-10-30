// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! A live voice caption: the tail of the current utterance, its streamed words
//! veiled in exactly like transcript text, and the previous utterance fading
//! out when the speaker turn changes.
use std::time::Instant;

use crate::{ElemVeil, VeilSpan, slice_spans, veil_opacity};

/// The previous utterance fades out over this long when the turn changes.
pub const CAPTION_EXIT_MS: f32 = 180.0;

/// Text tail of `text` holding at most `max` characters, ellipsized in front.
pub fn caption_tail(text: &str, max: usize) -> String {
    caption_window(text, max).0
}

/// The shown tail, the byte offset in `text` where it starts, and the length
/// of the ellipsis in front of it.
fn caption_window(text: &str, max: usize) -> (String, usize, usize) {
    let body = text.trim();
    let start = text.len().saturating_sub(text.trim_start().len());
    let count = body.chars().count();
    let Some(cut) = count.checked_sub(max).filter(|&cut| cut > 0) else {
        return (body.to_owned(), start, 0);
    };
    let (skip, tail) = body
        .char_indices()
        .nth(cut)
        .map_or((body.len(), ""), |(i, _)| (i, body.get(i..).unwrap_or_default()));
    let shown = tail.trim_start();
    let ellipsis = '…'.len_utf8();
    let from = start.saturating_add(skip).saturating_add(tail.len().saturating_sub(shown.len()));
    (format!("…{shown}"), from, ellipsis)
}

/// One frame of a caption.
#[derive(Debug, Clone, PartialEq)]
pub struct CaptionFrame {
    /// The caption as shown: the utterance's tail, ellipsized in front when cut.
    pub text: String,
    /// Veiled byte ranges of `text` and their opacity (0..1).
    pub spans: Vec<VeilSpan>,
    /// The previous utterance while it fades out, and its opacity.
    pub previous: Option<(String, f32)>,
}

/// Veil state for one call's caption. Advance it with the current utterance
/// on every frame while [`is_animating`](Self::is_animating).
#[derive(Debug)]
pub struct CaptionVeil {
    max_chars: usize,
    item: Option<String>,
    veil: ElemVeil,
    shown: String,
    outgoing: Option<(String, Instant)>,
}

impl CaptionVeil {
    /// A caption that shows at most `max_chars` characters of an utterance.
    pub fn new(max_chars: usize) -> Self {
        Self {
            max_chars,
            item: None,
            veil: ElemVeil::default(),
            shown: String::new(),
            outgoing: None,
        }
    }

    /// Advance to `text` of utterance `item` at `now`. A new item fades the
    /// previous caption out and the new one in from empty; a final that
    /// repeats its partials never re-animates. Reduced motion shows text
    /// directly but keeps tracking it, so turning motion back on mid-call
    /// does not re-fade what is already on screen.
    pub fn advance(
        &mut self,
        item: Option<&str>,
        text: &str,
        now: Instant,
        reduced_motion: bool,
    ) -> CaptionFrame {
        if item != self.item.as_deref() {
            let previous = std::mem::take(&mut self.shown);
            self.outgoing = (!previous.is_empty() && !reduced_motion).then_some((previous, now));
            self.item = item.map(str::to_owned);
            self.veil = ElemVeil::default();
        }
        let spans = self.veil.advance(text, now);
        let (shown, from, ellipsis) = caption_window(text, self.max_chars);
        let spans = if reduced_motion {
            Vec::new()
        } else {
            let end = from.saturating_add(shown.len().saturating_sub(ellipsis));
            slice_spans(&spans, from, end)
                .into_iter()
                .map(|(range, alpha)| {
                    (
                        range.start.saturating_add(ellipsis)..range.end.saturating_add(ellipsis),
                        alpha,
                    )
                })
                .collect()
        };
        self.shown.clone_from(&shown);
        let previous = self.outgoing.take().and_then(|(previous, started)| {
            let elapsed = now.saturating_duration_since(started).as_secs_f32() * 1000.0;
            let progress = elapsed / CAPTION_EXIT_MS;
            (progress < 1.0 && !reduced_motion).then(|| {
                self.outgoing = Some((previous.clone(), started));
                (previous, 1.0 - veil_opacity(progress))
            })
        });
        CaptionFrame { text: shown, spans, previous }
    }

    /// Something is still fading (as of the last advance): keep repainting.
    pub const fn is_animating(&self) -> bool {
        self.veil.is_fading() || self.outgoing.is_some()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn at(base: Instant, ms: u64) -> Instant {
        base.checked_add(Duration::from_millis(ms)).unwrap()
    }

    #[test]
    fn caption_keeps_the_latest_words_of_a_long_utterance() {
        assert_eq!(caption_tail("  short  ", 10), "short");
        assert_eq!(caption_tail("one two three four five", 9), "…four five");
        // Multibyte text is cut on character boundaries.
        assert_eq!(caption_tail("ñandú camión", 6), "…camión");
        assert_eq!(caption_tail("   ", 6), "");
    }

    #[test]
    fn streamed_words_fade_in_once_and_settle() {
        let t0 = Instant::now();
        let mut caption = CaptionVeil::new(160);
        let frame = caption.advance(Some("a"), "Hello", t0, false);
        assert_eq!(frame.text, "Hello");
        assert_eq!(frame.spans, vec![(0..5, 0.0)]);
        let frame = caption.advance(Some("a"), "Hello there", at(t0, 100), false);
        assert_eq!(frame.spans.len(), 2);
        assert_eq!(frame.spans[1].0, 5..11);
        assert!(caption.is_animating());
        // The final repeats the partials: nothing re-animates.
        let frame = caption.advance(Some("a"), "Hello there", at(t0, 900), false);
        assert_eq!(frame.spans, Vec::<VeilSpan>::new());
        assert!(!caption.is_animating());
    }

    #[test]
    fn veil_follows_the_visible_tail_of_a_long_utterance() {
        let t0 = Instant::now();
        let mut caption = CaptionVeil::new(9);
        caption.advance(Some("a"), "one two three four", t0, false);
        // Settle, then append: only the new word is veiled, at its position in
        // the ellipsized tail rather than the full utterance.
        caption.advance(Some("a"), "one two three four", at(t0, 900), false);
        let frame = caption.advance(Some("a"), "one two three four five", at(t0, 1000), false);
        assert_eq!(frame.text, "…four five");
        let ellipsis = '…'.len_utf8();
        assert_eq!(frame.spans, vec![(ellipsis + 4..ellipsis + 9, 0.0)]);
        assert_eq!(frame.text.get(frame.spans[0].0.clone()), Some(" five"));
    }

    #[test]
    fn a_new_turn_fades_the_previous_caption_out() {
        let t0 = Instant::now();
        let mut caption = CaptionVeil::new(160);
        caption.advance(Some("user"), "Open the sync test", t0, false);
        caption.advance(Some("user"), "Open the sync test", at(t0, 900), false);
        let frame = caption.advance(Some("assistant"), "On it", at(t0, 1000), false);
        assert_eq!(frame.spans, vec![(0..5, 0.0)], "the new turn fades in from empty");
        let (previous, alpha) = frame.previous.unwrap();
        assert_eq!(previous, "Open the sync test");
        assert_eq!(alpha, 1.0);
        let frame = caption.advance(Some("assistant"), "On it", at(t0, 1090), false);
        let (_, alpha) = frame.previous.unwrap();
        assert!(alpha > 0.0 && alpha < 1.0);
        let frame = caption.advance(Some("assistant"), "On it", at(t0, 1200), false);
        assert!(frame.previous.is_none());
    }

    #[test]
    fn reduced_motion_shows_text_directly_and_never_refades_it() {
        let t0 = Instant::now();
        let mut caption = CaptionVeil::new(160);
        caption.advance(Some("a"), "Hi", t0, false);
        let frame = caption.advance(Some("b"), "Hello", at(t0, 50), true);
        assert_eq!(frame.spans, Vec::<VeilSpan>::new());
        assert!(frame.previous.is_none());
        // Motion back on: the text already shown stays settled.
        let frame = caption.advance(Some("b"), "Hello", at(t0, 900), false);
        assert_eq!(frame.spans, Vec::<VeilSpan>::new());
    }
}
