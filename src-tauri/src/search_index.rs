use crate::crypto::CryptoManager;
use parking_lot::RwLock;
use sqlx::SqlitePool;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

type IndexedClipRow = (String, String, Vec<u8>, String, Option<String>);

#[derive(Clone, Debug, Default)]
struct SearchDocument {
    content: String,
    preview: String,
    ocr: String,
}

impl SearchDocument {
    fn new(content: &str, preview: &str, ocr: Option<&str>) -> Self {
        Self {
            content: normalize(content),
            preview: normalize(preview),
            ocr: normalize(ocr.unwrap_or_default()),
        }
    }

    fn contains(&self, query: &str) -> bool {
        self.content.contains(query) || self.preview.contains(query) || self.ocr.contains(query)
    }

    fn trigrams(&self) -> HashSet<String> {
        [&self.content, &self.preview, &self.ocr]
            .into_iter()
            .flat_map(|field| trigrams(field))
            .collect()
    }
}

#[derive(Default)]
struct IndexState {
    documents: HashMap<Arc<str>, SearchDocument>,
    postings: HashMap<String, HashSet<Arc<str>>>,
}

impl IndexState {
    fn insert(&mut self, id: String, document: SearchDocument) {
        self.remove(&id);
        let id: Arc<str> = Arc::from(id);
        for trigram in document.trigrams() {
            self.postings.entry(trigram).or_default().insert(id.clone());
        }
        self.documents.insert(id, document);
    }

    fn remove(&mut self, id: &str) {
        let Some(document) = self.documents.remove(id) else {
            return;
        };
        for trigram in document.trigrams() {
            let remove_posting = self.postings.get_mut(&trigram).is_some_and(|ids| {
                ids.remove(id);
                ids.is_empty()
            });
            if remove_posting {
                self.postings.remove(&trigram);
            }
        }
    }

    fn matches(&self, query: &str) -> HashSet<String> {
        let query = normalize(query);
        let query_trigrams = trigrams(&query);
        if query_trigrams.is_empty() {
            return self
                .documents
                .iter()
                .filter(|(_, document)| document.contains(&query))
                .map(|(id, _)| id.to_string())
                .collect();
        }

        let mut postings = query_trigrams
            .iter()
            .map(|trigram| self.postings.get(trigram))
            .collect::<Vec<_>>();
        if postings.iter().any(|posting| posting.is_none()) {
            return HashSet::new();
        }
        postings.sort_by_key(|posting| posting.map_or(0, HashSet::len));

        let mut candidates = postings[0].cloned().unwrap_or_default().clone();
        for posting in postings.into_iter().skip(1).flatten() {
            candidates.retain(|id| posting.contains(id));
            if candidates.is_empty() {
                break;
            }
        }
        candidates.retain(|id| {
            self.documents
                .get(id)
                .is_some_and(|document| document.contains(&query))
        });
        candidates.into_iter().map(|id| id.to_string()).collect()
    }
}

#[derive(Default)]
pub struct SearchIndex {
    state: RwLock<Option<IndexState>>,
    generation: AtomicU64,
    rebuild: Mutex<()>,
}

impl SearchIndex {
    pub async fn ensure_ready(
        &self,
        pool: &SqlitePool,
        crypto: &CryptoManager,
    ) -> Result<(), String> {
        if self.state.read().is_some() {
            return Ok(());
        }

        let _rebuild = self.rebuild.lock().await;
        loop {
            if self.state.read().is_some() {
                return Ok(());
            }
            let generation = self.generation.load(Ordering::Acquire);
            let clips: Vec<IndexedClipRow> = sqlx::query_as(
                r#"
                SELECT uuid,
                       clip_type,
                       CASE WHEN clip_type = 'image' THEN x'' ELSE content END,
                       text_preview,
                       ocr_text
                FROM clips
                WHERE is_deleted = 0
                "#,
            )
            .fetch_all(pool)
            .await
            .map_err(|error| error.to_string())?;
            let mut next = IndexState::default();
            for (id, clip_type, encrypted_content, encrypted_preview, encrypted_ocr) in clips {
                let content = if clip_type == "image" {
                    Vec::new()
                } else {
                    crypto.decrypt(&encrypted_content)?
                };
                let preview = crypto.decrypt_text(&encrypted_preview)?;
                let ocr = encrypted_ocr
                    .as_deref()
                    .map(|value| crypto.decrypt_text(value))
                    .transpose()?;
                let searchable_content = if clip_type != "image" {
                    String::from_utf8_lossy(&content).into_owned()
                } else {
                    String::new()
                };
                next.insert(
                    id,
                    SearchDocument::new(&searchable_content, &preview, ocr.as_deref()),
                );
            }

            if self.generation.load(Ordering::Acquire) == generation {
                let count = next.documents.len();
                *self.state.write() = Some(next);
                log::info!("SEARCH: Built encrypted-safe in-memory index for {count} clips");
                return Ok(());
            }
        }
    }

    pub fn matches(&self, query: &str) -> HashSet<String> {
        self.state
            .read()
            .as_ref()
            .map_or_else(HashSet::new, |state| state.matches(query))
    }

    pub fn upsert(
        &self,
        id: &str,
        clip_type: &str,
        content: &[u8],
        preview: &str,
        ocr: Option<&str>,
    ) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        if let Some(state) = self.state.write().as_mut() {
            let preserved_ocr = state
                .documents
                .get(id)
                .map(|document| document.ocr.clone())
                .unwrap_or_default();
            let searchable_content = if clip_type != "image" {
                String::from_utf8_lossy(content).into_owned()
            } else {
                String::new()
            };
            let mut document = SearchDocument::new(&searchable_content, preview, ocr);
            if ocr.is_none() {
                document.ocr = preserved_ocr;
            }
            state.insert(id.to_string(), document);
        }
    }

    pub fn update_ocr(&self, id: &str, ocr: &str) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        if let Some(state) = self.state.write().as_mut() {
            if let Some(mut document) = state.documents.get(id).cloned() {
                document.ocr = normalize(ocr);
                state.insert(id.to_string(), document);
            }
        }
    }

    pub fn remove(&self, id: &str) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        if let Some(state) = self.state.write().as_mut() {
            state.remove(id);
        }
    }

    pub fn invalidate(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        *self.state.write() = None;
    }
}

fn normalize(value: &str) -> String {
    value.to_lowercase()
}

fn trigrams(value: &str) -> HashSet<String> {
    let characters = value.chars().collect::<Vec<_>>();
    characters
        .windows(3)
        .map(|window| window.iter().collect())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_exact_substrings_case_insensitively() {
        let mut state = IndexState::default();
        state.insert(
            "one".to_string(),
            SearchDocument::new("Release confirmation 4J7K", "", None),
        );
        state.insert(
            "two".to_string(),
            SearchDocument::new("unrelated", "", None),
        );

        assert_eq!(
            state.matches("CONFIRMATION 4j7"),
            HashSet::from(["one".to_string()])
        );
    }

    #[test]
    fn supports_short_queries_and_ocr_updates() {
        let index = SearchIndex::default();
        *index.state.write() = Some(IndexState::default());
        index.upsert("image", "image", &[], "Screenshot", None);
        assert!(index.matches("sc").contains("image"));

        index.update_ocr("image", "The clipboard service is unavailable");
        assert!(index.matches("service is unavailable").contains("image"));
        index.upsert("image", "image", &[], "Screenshot", None);
        assert!(index.matches("service is unavailable").contains("image"));
        index.remove("image");
        assert!(index.matches("service").is_empty());
    }
}
