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
        self.buf.drain(..).collect()
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
    fn oversized_write_keeps_its_tail() {
        let mut r = Ring::new(3);
        r.push(b"xy");
        r.push(b"abcdef");
        assert_eq!(r.len(), 3);
        assert_eq!(r.drain(), b"def");
        assert_eq!(r.dropped(), 5);
    }
}
