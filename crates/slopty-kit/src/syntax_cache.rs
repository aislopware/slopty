// Derived from zeron (MIT, github.com/zeronsh/zeron @ 053ce2e), see THIRD_PARTY_NOTICES.md
//! A bounded cache of highlighted documents, keyed by content.
//!
//! Colours and GPUI runs stay outside it on purpose: a change of appearance recolours the spans
//! it holds without parsing again.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use crate::syntax::{HighlightedDocument, LanguageId};

/// Bumped when the highlighting itself changes, so old documents miss.
pub const QUERY_GENERATION: u32 = 1;
/// The most documents held.
pub const MAX_DOCUMENTS: usize = 96;
/// The most span bytes held.
pub const MAX_RETAINED_BYTES: usize = 24 * 1024 * 1024;

/// What a highlighted document is cached under.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DocumentHighlightKey {
    /// The grammar.
    pub language: LanguageId,
    /// The source's BLAKE3 hash.
    pub content_hash: [u8; 32],
    /// [`QUERY_GENERATION`] when it was highlighted.
    pub query_generation: u32,
}

impl DocumentHighlightKey {
    /// The key for `source` in `language`.
    #[must_use]
    pub fn new(language: LanguageId, source: &str) -> Self {
        Self {
            language,
            content_hash: *blake3::hash(source.as_bytes()).as_bytes(),
            query_generation: QUERY_GENERATION,
        }
    }
}

#[derive(Debug)]
struct CachedDocument {
    retained_bytes: usize,
    document: Arc<HighlightedDocument>,
}

/// The cache: at most [`MAX_DOCUMENTS`] documents and [`MAX_RETAINED_BYTES`] of spans, least
/// recently used out first.
#[derive(Debug, Default)]
pub struct SyntaxHighlightCache {
    documents: HashMap<DocumentHighlightKey, CachedDocument>,
    recency: VecDeque<DocumentHighlightKey>,
    retained_bytes: usize,
    hits: u64,
    misses: u64,
}

/// The cache's counts, for measurements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyntaxCacheStats {
    /// Lookups that found a document.
    pub hits: u64,
    /// Lookups that did not.
    pub misses: u64,
    /// Documents held.
    pub documents: usize,
    /// Their estimated bytes.
    pub retained_bytes: usize,
}

impl SyntaxHighlightCache {
    /// The document under `key`, marked as just used.
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

    /// Hold `document` under `key`; false when it alone is over the byte budget.
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

    /// The counts.
    #[must_use]
    pub fn stats(&self) -> SyntaxCacheStats {
        SyntaxCacheStats {
            hits: self.hits,
            misses: self.misses,
            documents: self.documents.len(),
            retained_bytes: self.retained_bytes,
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.documents.len()
    }
}

fn estimated_document_bytes(document: &HighlightedDocument) -> usize {
    size_of::<HighlightedDocument>()
        .saturating_add(
            document
                .lines
                .capacity()
                .saturating_mul(size_of::<Vec<crate::syntax::HighlightSpan>>()),
        )
        .saturating_add(document.lines.iter().fold(0_usize, |total, line| {
            total.saturating_add(
                line.capacity().saturating_mul(size_of::<crate::syntax::HighlightSpan>()),
            )
        }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::language_for_alias;

    fn rust() -> LanguageId {
        language_for_alias("rust").unwrap()
    }

    #[test]
    fn key_depends_on_content_language_and_query_not_theme() {
        let key = DocumentHighlightKey::new(rust(), "fn main() {}");
        assert_eq!(key, DocumentHighlightKey::new(rust(), "fn main() {}"));
        assert_ne!(key, DocumentHighlightKey::new(rust(), "fn other() {}"));
        assert_ne!(
            key,
            DocumentHighlightKey::new(language_for_alias("typescript").unwrap(), "fn main() {}")
        );
    }

    #[test]
    fn cache_reuses_neutral_documents() {
        let source = "fn main() {}";
        let key = DocumentHighlightKey::new(rust(), source);
        let document = Arc::new(
            crate::syntax::highlight(crate::syntax::HighlightRequest {
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
        let key = DocumentHighlightKey::new(rust(), source);
        let document = Arc::new(
            crate::syntax::highlight(crate::syntax::HighlightRequest {
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
