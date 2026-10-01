//! What a session's terminal holds in memory, and compressing its history while it is idle.
//!
//! libghostty stores the screens and their scrollback in fixed-size pages and can compress the
//! pages of history in place; a page read again (a scrollback fetch, a search, a checkpoint)
//! comes back transparently. It counts its own pages, so the worker can see what a session
//! costs and what compression saved.

use libghostty_vt::terminal::{CompressionMode, CompressionResult};

use super::GhosttyEngine;
use crate::EngineError;

/// Memory a session's terminal holds, both screens together, as libghostty counts its pages.
/// Colours, styles and hyperlinks live in the pages; images are counted apart.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Memory {
    /// Physical memory the pages use, a compressed page at its compressed size: the figure to
    /// budget against.
    pub resident_bytes: u64,
    /// Address space reserved for the pages, which compression does not give back.
    pub virtual_bytes: u64,
    /// Pages, compressed ones included.
    pub pages: u64,
    /// Pages that are compressed.
    pub compressed_pages: u64,
    /// Compressed data held for them, already in `resident_bytes`.
    pub compressed_bytes: u64,
    /// Kitty graphics image data, not in `resident_bytes`.
    pub image_bytes: u64,
    /// Whether compressing history can free memory on this platform.
    pub compression_supported: bool,
}

/// Where compressing a session's history stands after a step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compression {
    /// More pages wait: step again while the session stays idle.
    Pending,
    /// Nothing is left to compress until the terminal changes.
    Done,
    /// Compression cannot free memory on this platform.
    Unsupported,
}

impl GhosttyEngine {
    /// What the terminal holds in memory now. It looks at every page without decompressing
    /// any, so it is for a periodic look, not every write.
    ///
    /// # Errors
    ///
    /// libghostty-vt failing.
    pub fn memory(&self) -> Result<Memory, EngineError> {
        let usage = self.term.memory_usage()?;
        let (primary, alternate) = (usage.primary, usage.alternate);
        Ok(Memory {
            resident_bytes: usage.resident_bytes(),
            virtual_bytes: primary.virtual_bytes.saturating_add(alternate.virtual_bytes),
            pages: primary.pages.saturating_add(alternate.pages),
            compressed_pages: primary.compressed_pages.saturating_add(alternate.compressed_pages),
            compressed_bytes: primary.compressed_bytes.saturating_add(alternate.compressed_bytes),
            image_bytes: primary.image_bytes.saturating_add(alternate.image_bytes),
            compression_supported: usage.compression_supported,
        })
    }

    /// Whether the last pass compressing the history left nothing for another: the terminal
    /// has not changed since, and nothing read the history back.
    ///
    /// # Errors
    ///
    /// libghostty-vt failing.
    pub fn history_compressed(&self) -> Result<bool, EngineError> {
        Ok(self.compressed_at.get() == Some(self.term.compression_activity()?))
    }

    /// One bounded step of compressing the history, for a session with nothing to do. Once a
    /// pass is done, steps cost nothing until the terminal changes again (libghostty's activity
    /// token), so the caller can keep stepping on its idle ticks.
    ///
    /// # Errors
    ///
    /// libghostty-vt failing.
    pub fn compress_history(&mut self) -> Result<Compression, EngineError> {
        if self.history_compressed()? {
            return Ok(Compression::Done);
        }
        Ok(match self.term.compress(CompressionMode::Incremental)? {
            CompressionResult::Pending => Compression::Pending,
            CompressionResult::Complete => {
                self.compressed_at.set(Some(self.term.compression_activity()?));
                Compression::Done
            }
            CompressionResult::Unsupported => Compression::Unsupported,
        })
    }
}

#[cfg(test)]
mod tests;
