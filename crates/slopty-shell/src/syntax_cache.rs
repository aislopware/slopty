// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! Bounded cache for neutral syntax documents.
//!
//! Colors and GPUI runs deliberately stay outside this cache so appearance
//! changes recolor existing spans without parsing again.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use sha2::{Digest as _, Sha256};
use slopty_syntax::{HighlightedDocument, LanguageId};

/// The highlight queries' version: a change to them re-parses every cached document.
pub const QUERY_GENERATION: u32 = 1;
const MAX_DOCUMENTS: usize = 96;
const MAX_RETAINED_BYTES: usize = 24 * 1024 * 1024;

/// What a highlighted document is cached under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DocumentHighlightKey {
    /// The language it was parsed as.
    pub language: LanguageId,
    /// SHA-256 of its source.
    pub content_hash: [u8; 32],
    /// The [`QUERY_GENERATION`] it was highlighted under.
    pub query_generation: u32,
}

impl DocumentHighlightKey {
    /// The key for `source` parsed as `language` under today's queries.
    pub fn new(language: LanguageId, source: &str) -> Self {
        Self {
            language,
            content_hash: Sha256::digest(source.as_bytes()).into(),
            query_generation: QUERY_GENERATION,
        }
    }
}

struct CachedDocument {
    retained_bytes: usize,
    document: Arc<HighlightedDocument>,
}

/// Highlighted documents by key, least recently used dropped first, bounded in count and bytes.
#[derive(Default)]
pub struct SyntaxHighlightCache {
    documents: HashMap<DocumentHighlightKey, CachedDocument>,
    recency: VecDeque<DocumentHighlightKey>,
    retained_bytes: usize,
    hits: u64,
    misses: u64,
}

impl std::fmt::Debug for SyntaxHighlightCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyntaxHighlightCache")
            .field("documents", &self.documents.len())
            .field("retained_bytes", &self.retained_bytes)
            .field("hits", &self.hits)
            .field("misses", &self.misses)
            .finish_non_exhaustive()
    }
}

/// The cache's counters, for measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntaxCacheStats {
    /// Lookups that found their document.
    pub hits: u64,
    /// Lookups that did not.
    pub misses: u64,
    /// Documents held.
    pub documents: usize,
    /// Their estimated size in bytes.
    pub retained_bytes: usize,
}

impl SyntaxHighlightCache {
    /// The document cached under `key`, now the most recently used.
    pub fn get(&mut self, key: &DocumentHighlightKey) -> Option<Arc<HighlightedDocument>> {
        let Some(document) = self.documents.get(key).map(|entry| Arc::clone(&entry.document))
        else {
            self.misses = self.misses.saturating_add(1);
            return None;
        };
        self.hits = self.hits.saturating_add(1);
        self.touch(*key);
        Some(document)
    }

    /// Caches `document` under `key`, dropping the least recently used past the bounds.
    ///
    /// Returns whether it stayed: a document larger than the whole byte budget is never kept.
    pub fn insert(
        &mut self,
        key: DocumentHighlightKey,
        document: Arc<HighlightedDocument>,
    ) -> bool {
        if let Some(previous) = self.documents.remove(&key) {
            self.retained_bytes = self.retained_bytes.saturating_sub(previous.retained_bytes);
        }
        let retained_bytes = estimated_document_bytes(&document);
        if retained_bytes > MAX_RETAINED_BYTES {
            self.recency.retain(|candidate| *candidate != key);
            return false;
        }
        self.retained_bytes = self.retained_bytes.saturating_add(retained_bytes);
        self.documents.insert(key, CachedDocument { retained_bytes, document });
        self.touch(key);
        while self.documents.len() > MAX_DOCUMENTS || self.retained_bytes > MAX_RETAINED_BYTES {
            let Some(oldest) = self.recency.pop_front() else {
                break;
            };
            if let Some(removed) = self.documents.remove(&oldest) {
                self.retained_bytes = self.retained_bytes.saturating_sub(removed.retained_bytes);
            }
        }
        self.documents.contains_key(&key)
    }

    fn touch(&mut self, key: DocumentHighlightKey) {
        self.recency.retain(|candidate| *candidate != key);
        self.recency.push_back(key);
    }

    /// The counters so far.
    pub fn stats(&self) -> SyntaxCacheStats {
        SyntaxCacheStats {
            hits: self.hits,
            misses: self.misses,
            documents: self.documents.len(),
            retained_bytes: self.retained_bytes,
        }
    }

    /// Documents held.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.documents.len()
    }

    /// Whether it holds none.
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.documents.is_empty()
    }
}

fn estimated_document_bytes(document: &HighlightedDocument) -> usize {
    size_of::<HighlightedDocument>()
        .saturating_add(
            document
                .lines
                .capacity()
                .saturating_mul(size_of::<Vec<slopty_syntax::HighlightSpan>>()),
        )
        .saturating_add(document.lines.iter().fold(0_usize, |total, line| {
            total.saturating_add(
                line.capacity().saturating_mul(size_of::<slopty_syntax::HighlightSpan>()),
            )
        }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_depends_on_content_language_and_query_not_theme() {
        let rust = DocumentHighlightKey::new(LanguageId::Rust, "fn main() {}");
        assert_eq!(rust, DocumentHighlightKey::new(LanguageId::Rust, "fn main() {}"));
        assert_ne!(rust, DocumentHighlightKey::new(LanguageId::Rust, "fn other() {}"));
        assert_ne!(rust, DocumentHighlightKey::new(LanguageId::TypeScript, "fn main() {}"));
    }

    #[test]
    fn cache_reuses_neutral_documents() {
        let source = "fn main() {}";
        let key = DocumentHighlightKey::new(LanguageId::Rust, source);
        let document = Arc::new(
            slopty_syntax::highlight(slopty_syntax::HighlightRequest {
                source,
                path: None,
                fence_tag: Some("rust"),
            })
            .unwrap(),
        );
        let mut cache = SyntaxHighlightCache::default();
        assert!(cache.insert(key, Arc::clone(&document)));
        assert!(Arc::ptr_eq(&cache.get(&key).unwrap(), &document));
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.stats().hits, 1);
        assert_eq!(cache.stats().misses, 0);
    }

    #[test]
    fn retained_bytes_measure_materialized_spans_not_source_text() {
        let source = "let x = 1;";
        let key = DocumentHighlightKey::new(LanguageId::Rust, source);
        let document = Arc::new(
            slopty_syntax::highlight(slopty_syntax::HighlightRequest {
                source,
                path: None,
                fence_tag: Some("rust"),
            })
            .unwrap(),
        );
        let mut cache = SyntaxHighlightCache::default();
        assert!(cache.insert(key, document));
        assert!(cache.stats().retained_bytes > source.len());
    }
}
