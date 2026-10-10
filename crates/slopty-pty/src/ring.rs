//! Bounded byte ring for detached output.

use std::collections::VecDeque;

/// Keeps the newest `capacity` bytes; older bytes fall off the front. Counts what it dropped so
/// the consumer knows its replay starts mid-stream.
#[derive(Clone, Debug)]
pub struct Ring {
    buf: VecDeque<u8>,
    capacity: usize,
    dropped: u64,
}

impl Ring {
    /// Ring holding at most `capacity` bytes.
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self { buf: VecDeque::with_capacity(capacity.min(1 << 16)), capacity, dropped: 0 }
    }

    /// Ring holding at most `capacity` bytes, holding `bytes` (their newest `capacity`) with
    /// `dropped` bytes already lost before them: a ring handed from one ptyd to its next build.
    #[must_use]
    pub fn holding(capacity: usize, bytes: &[u8], dropped: u64) -> Self {
        let mut ring = Self::new(capacity);
        ring.push(bytes);
        ring.dropped = ring.dropped.saturating_add(dropped);
        ring
    }

    /// A copy of everything held, which stays held.
    #[must_use]
    pub fn contents(&self) -> Vec<u8> {
        self.buf.iter().copied().collect()
    }

    /// Append, evicting from the front as needed.
    pub fn push(&mut self, bytes: &[u8]) {
        if bytes.len() >= self.capacity {
            // Only the tail of this write can survive.
            let skip = bytes.len().saturating_sub(self.capacity);
            self.dropped =
                self.dropped.saturating_add(self.buf.len() as u64).saturating_add(skip as u64);
            self.buf.clear();
            self.buf.extend(bytes.get(skip..).unwrap_or_default());
            return;
        }
        let overflow = self.buf.len().saturating_add(bytes.len()).saturating_sub(self.capacity);
        if overflow > 0 {
            self.buf.drain(..overflow);
            self.dropped = self.dropped.saturating_add(overflow as u64);
        }
        self.buf.extend(bytes);
    }

    /// Take everything out.
    pub fn drain(&mut self) -> Vec<u8> {
        Vec::from(std::mem::take(&mut self.buf))
    }

    /// Forget everything, including the count of what was dropped: the bytes are accounted
    /// for elsewhere (a checkpoint holds their effect).
    pub fn clear(&mut self) {
        self.buf.clear();
        self.dropped = 0;
    }

    /// Bytes currently held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Nothing held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Total bytes evicted since creation.
    #[must_use]
    pub const fn dropped(&self) -> u64 {
        self.dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_newest_bytes() {
        let mut r = Ring::new(4);
        r.push(b"ab");
        r.push(b"cd");
        r.push(b"e");
        assert_eq!(r.drain(), b"bcde");
        assert_eq!(r.dropped(), 1);
        assert!(r.is_empty());
    }

    #[test]
    fn clear_forgets_the_bytes_and_the_drop_count() {
        let mut r = Ring::new(2);
        r.push(b"abc");
        assert_eq!(r.dropped(), 1);
        r.clear();
        assert!(r.is_empty());
        assert_eq!(r.dropped(), 0);
        r.push(b"z");
        assert_eq!(r.drain(), b"z");
    }

    /// A ring handed on holds what it was given and counts what was lost before it, and a
    /// copy of it leaves it as it was.
    #[test]
    fn a_handed_ring_keeps_its_bytes_and_its_losses() {
        let r = Ring::holding(3, b"abcd", 5);
        assert_eq!(r.contents(), b"bcd");
        assert_eq!(r.dropped(), 6, "the 5 lost before, and the 1 that did not fit");
        assert_eq!(r.len(), 3, "a copy takes nothing out");
    }

    #[test]
    fn oversized_write_keeps_its_tail() {
        let mut r = Ring::new(3);
        r.push(b"xy");
        r.push(b"abcdef");
        assert_eq!(r.len(), 3);
        assert_eq!(r.drain(), b"def");
        assert_eq!(r.dropped(), 5);
    }
}
