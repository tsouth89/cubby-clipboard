use crate::crypto::CryptoManager;
use parking_lot::RwLock;
use sqlx::SqlitePool;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

type IndexedClipRow = (
    String,
    String,
    Vec<u8>,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

#[derive(Clone, Debug, Default)]
struct SearchDocument {
    content: String,
    preview: String,
    ocr: String,
    /// The user's note for this clip (SOU-588). Searchable alongside the
    /// content and OCR text, which is the point of writing one.
    notes: String,
    /// Source app exactly as captured, for the History window's per-app filter
    /// and counts. `source_app` is encrypted with a random nonce, so SQL can
    /// neither group nor filter on it; this index already holds the decrypted
    /// view of every clip, so the answer lives here rather than in a second
    /// table or a parallel index with its own lifecycle to keep in sync.
    ///
    /// Deliberately not part of `contains`/`trigrams`: searching "chrome"
    /// should find clips whose *text* says chrome, not every clip copied from
    /// the browser.
    source_app: Option<String>,
}

impl SearchDocument {
    fn new(
        content: &str,
        preview: &str,
        ocr: Option<&str>,
        notes: Option<&str>,
        source_app: Option<&str>,
    ) -> Self {
        let content = normalize(content);
        Self {
            preview: redundant_preview(&content, &normalize(preview)),
            content,
            ocr: normalize(ocr.unwrap_or_default()),
            notes: normalize(notes.unwrap_or_default()),
            source_app: source_app
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string),
        }
    }

    fn contains(&self, query: &str) -> bool {
        self.content.contains(query)
            || self.preview.contains(query)
            || self.ocr.contains(query)
            || self.notes.contains(query)
    }

    fn trigrams(&self) -> HashSet<String> {
        [&self.content, &self.preview, &self.ocr, &self.notes]
            .into_iter()
            .flat_map(|field| trigrams(field))
            .collect()
    }
}

/// A document plus the id it was indexed under.
struct IndexedDocument {
    id: Arc<str>,
    document: SearchDocument,
}

/// Slot number for a document. Postings store these rather than clip ids.
///
/// A `HashSet<Arc<str>>` per trigram costs a 16-byte fat pointer plus the set's
/// own per-slot overhead for every posting, and a clip of ordinary prose
/// produces well over a hundred trigrams -- which made postings, not the stored
/// text, the dominant cost of the index. A `u32` in a sorted `Vec` is four bytes
/// with no per-entry overhead, and intersects faster besides.
type Slot = u32;

#[derive(Default)]
struct IndexState {
    /// Indexed by [`Slot`]. A removed document leaves a hole, which the next
    /// insert reuses, so slots stay dense and postings stay small.
    documents: Vec<Option<IndexedDocument>>,
    slots: HashMap<Arc<str>, Slot>,
    free_slots: Vec<Slot>,
    /// Sorted, deduplicated slots per trigram. Sorted so intersection is a
    /// linear merge rather than a hash lookup per candidate.
    postings: HashMap<Box<str>, Vec<Slot>>,
}

impl IndexState {
    fn document(&self, id: &str) -> Option<&SearchDocument> {
        let slot = *self.slots.get(id)?;
        self.documents
            .get(slot as usize)
            .and_then(Option::as_ref)
            .map(|entry| &entry.document)
    }

    fn documents(&self) -> impl Iterator<Item = (&Arc<str>, &SearchDocument)> {
        self.documents
            .iter()
            .flatten()
            .map(|entry| (&entry.id, &entry.document))
    }

    fn len(&self) -> usize {
        self.slots.len()
    }

    fn insert(&mut self, id: String, document: SearchDocument) {
        self.remove(&id);

        let id: Arc<str> = Arc::from(id);
        let slot = match self.free_slots.pop() {
            Some(slot) => slot,
            None => {
                self.documents.push(None);
                (self.documents.len() - 1) as Slot
            }
        };

        for trigram in document.trigrams() {
            let postings = self.postings.entry(trigram.into_boxed_str()).or_default();
            // Kept sorted on insert so `matches` can merge without sorting.
            if let Err(position) = postings.binary_search(&slot) {
                postings.insert(position, slot);
            }
        }

        self.slots.insert(id.clone(), slot);
        self.documents[slot as usize] = Some(IndexedDocument { id, document });
    }

    fn remove(&mut self, id: &str) {
        let Some(slot) = self.slots.remove(id) else {
            return;
        };
        let Some(entry) = self.documents[slot as usize].take() else {
            return;
        };

        for trigram in entry.document.trigrams() {
            let empty = self
                .postings
                .get_mut(trigram.as_str())
                .is_some_and(|slots| {
                    if let Ok(position) = slots.binary_search(&slot) {
                        slots.remove(position);
                    }
                    slots.is_empty()
                });
            if empty {
                self.postings.remove(trigram.as_str());
            }
        }

        self.free_slots.push(slot);
        self.compact_if_sparse();
    }

    /// Reclaim slots once most of them are holes.
    ///
    /// A freed slot still costs its `Option<IndexedDocument>` header in the
    /// `documents` vector plus four bytes in `free_slots`, and nothing ever
    /// returned that memory: a history that peaked at 100k clips and was then
    /// trimmed to 1k kept 100k slot headers, which is the per-clip budget
    /// missed by an order of magnitude in the steady state after a cleanup.
    ///
    /// Truncating the tail would not help, because retention prunes the
    /// *oldest* clips and those hold the *lowest* slots. So this compacts
    /// properly. It is O(documents + postings), which is why it only runs when
    /// the majority of slots are free -- and because new slots are assigned in
    /// increasing order of old slot, the remap is monotonic and posting lists
    /// stay sorted without re-sorting them.
    fn compact_if_sparse(&mut self) {
        /// Below this, compaction is not worth the churn -- and it is deliberately
        /// small, because the threshold is also the floor on how many empty slots
        /// can be left behind. At 256 a trimmed history settled at 249 slots for
        /// 10 clips; at 16 it keeps shrinking until the waste is negligible.
        const MIN_SLOTS: usize = 16;

        if self.documents.len() < MIN_SLOTS || self.free_slots.len() * 2 <= self.documents.len() {
            return;
        }

        let mut remap = vec![None; self.documents.len()];
        let mut compacted = Vec::with_capacity(self.slots.len());
        for (old_slot, entry) in self.documents.drain(..).enumerate() {
            if let Some(entry) = entry {
                remap[old_slot] = Some(compacted.len() as Slot);
                compacted.push(Some(entry));
            }
        }
        self.documents = compacted;

        for slot in self.slots.values_mut() {
            *slot = remap[*slot as usize].expect("a live document keeps its slot");
        }

        for postings in self.postings.values_mut() {
            postings.retain_mut(|slot| match remap[*slot as usize] {
                Some(new_slot) => {
                    *slot = new_slot;
                    true
                }
                None => false,
            });
        }

        self.free_slots.clear();
        self.documents.shrink_to_fit();
        self.free_slots.shrink_to_fit();
    }

    fn matches(&self, query: &str) -> HashSet<String> {
        let query = normalize(query);
        let query_trigrams = trigrams(&query);
        if query_trigrams.is_empty() {
            // Queries shorter than a trigram cannot use the index at all.
            return self
                .documents()
                .filter(|(_, document)| document.contains(&query))
                .map(|(id, _)| id.to_string())
                .collect();
        }

        let mut postings = Vec::with_capacity(query_trigrams.len());
        for trigram in &query_trigrams {
            match self.postings.get(trigram.as_str()) {
                // A trigram nothing has means nothing can match all of them.
                None => return HashSet::new(),
                Some(slots) => postings.push(slots.as_slice()),
            }
        }
        // Start from the rarest trigram so the first intersection is as small
        // as possible.
        postings.sort_by_key(|slots| slots.len());

        let mut candidates = postings[0].to_vec();
        for slots in postings.into_iter().skip(1) {
            candidates = intersect_sorted(&candidates, slots);
            if candidates.is_empty() {
                return HashSet::new();
            }
        }

        // Trigram agreement is necessary but not sufficient -- the trigrams of
        // a query can all be present in a document in the wrong order -- so the
        // surviving candidates are still checked for the literal substring.
        candidates
            .into_iter()
            .filter_map(|slot| self.documents[slot as usize].as_ref())
            .filter(|entry| entry.document.contains(&query))
            .map(|entry| entry.id.to_string())
            .collect()
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
                       ocr_text,
                       notes,
                       source_app
                FROM clips
                WHERE is_deleted = 0
                "#,
            )
            .fetch_all(pool)
            .await
            .map_err(|error| error.to_string())?;
            let mut next = IndexState::default();
            for (
                id,
                clip_type,
                encrypted_content,
                encrypted_preview,
                encrypted_ocr,
                encrypted_notes,
                encrypted_source_app,
            ) in clips
            {
                let content = if clip_type == "image" {
                    Vec::new()
                } else {
                    crypto.decrypt(&encrypted_content)?
                };
                let preview = crypto.decrypt_text(&encrypted_preview)?;
                let ocr = encrypted_ocr.as_deref().and_then(|value| {
                    crypto
                        .decrypt_text(value)
                        .map_err(|error| {
                            log::warn!("SEARCH: Ignoring unreadable auxiliary OCR text: {error}")
                        })
                        .ok()
                });
                let notes = encrypted_notes.as_deref().and_then(|value| {
                    crypto
                        .decrypt_text(value)
                        .map_err(|error| log::warn!("SEARCH: Ignoring an unreadable note: {error}"))
                        .ok()
                });
                // Like OCR text, the source app is auxiliary: an unreadable one
                // must not stop the clip being indexed and searchable.
                let source_app = encrypted_source_app.as_deref().and_then(|value| {
                    crypto
                        .decrypt_text(value)
                        .map_err(|error| {
                            log::warn!("SEARCH: Ignoring unreadable source app: {error}")
                        })
                        .ok()
                });
                let searchable_content = if clip_type != "image" {
                    String::from_utf8_lossy(&content).into_owned()
                } else {
                    String::new()
                };
                next.insert(
                    id,
                    SearchDocument::new(
                        &searchable_content,
                        &preview,
                        ocr.as_deref(),
                        notes.as_deref(),
                        source_app.as_deref(),
                    ),
                );
            }

            // Hold the write lock while rechecking the generation so a mutation
            // cannot slip in between the check and the publish and get lost.
            let mut state = self.state.write();
            if self.generation.load(Ordering::Acquire) == generation {
                let count = next.len();
                *state = Some(next);
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

    /// Every source app present in the history with its clip count, most used
    /// first, then alphabetically so the list is stable between refreshes.
    pub fn source_app_counts(&self) -> Vec<(String, usize)> {
        let state = self.state.read();
        let Some(state) = state.as_ref() else {
            return Vec::new();
        };
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for (_, document) in state.documents() {
            if let Some(app) = document.source_app.as_deref() {
                *counts.entry(app).or_default() += 1;
            }
        }
        let mut counts: Vec<(String, usize)> = counts
            .into_iter()
            .map(|(app, count)| (app.to_string(), count))
            .collect();
        counts.sort_by(|(left_app, left_count), (right_app, right_count)| {
            right_count
                .cmp(left_count)
                .then_with(|| left_app.to_lowercase().cmp(&right_app.to_lowercase()))
        });
        counts
    }

    /// Ids of every clip captured from `app`, compared case-insensitively so a
    /// filter picked from the list matches regardless of how Windows reported
    /// the executable name at capture time.
    pub fn ids_for_source_app(&self, app: &str) -> HashSet<String> {
        let wanted = app.trim().to_lowercase();
        let state = self.state.read();
        state.as_ref().map_or_else(HashSet::new, |state| {
            state
                .documents()
                .filter(|(_, document)| {
                    document
                        .source_app
                        .as_deref()
                        .is_some_and(|value| value.to_lowercase() == wanted)
                })
                .map(|(id, _)| id.to_string())
                .collect()
        })
    }

    pub fn upsert(
        &self,
        id: &str,
        clip_type: &str,
        content: &[u8],
        preview: &str,
        ocr: Option<&str>,
        source_app: Option<&str>,
    ) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        if let Some(state) = self.state.write().as_mut() {
            let existing = state.document(id).cloned().unwrap_or_default();
            let searchable_content = if clip_type != "image" {
                String::from_utf8_lossy(content).into_owned()
            } else {
                String::new()
            };
            let mut document =
                SearchDocument::new(&searchable_content, preview, ocr, None, source_app);
            if ocr.is_none() {
                document.ocr = existing.ocr;
            }
            // Notes are only ever written through update_notes, so an upsert
            // from a re-capture must not silently drop one.
            document.notes = existing.notes;
            if source_app.is_none() {
                document.source_app = existing.source_app;
            }
            state.insert(id.to_string(), document);
        }
    }

    /// Replace a clip's note text in the index.
    pub fn update_notes(&self, id: &str, notes: &str) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        if let Some(state) = self.state.write().as_mut() {
            if let Some(mut document) = state.document(id).cloned() {
                document.notes = normalize(notes);
                state.insert(id.to_string(), document);
            }
        }
    }

    pub fn update_ocr(&self, id: &str, ocr: &str) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        if let Some(state) = self.state.write().as_mut() {
            if let Some(mut document) = state.document(id).cloned() {
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

/// Drop a preview that the content already contains.
///
/// `text_preview` is a truncated copy of the clip's own text, so for a text
/// clip it is a substring of `content` and storing it doubles the resident cost
/// of the clip while adding nothing searchable: both `contains` and `trigrams`
/// take the union of the fields, and the union with a subset is the set.
///
/// Image clips are the case that keeps this field alive at all -- their content
/// is empty and the preview is the only text they have -- as is any preview
/// that is not a clean prefix, such as one with an ellipsis appended.
fn redundant_preview(content: &str, preview: &str) -> String {
    if preview.is_empty() || content.contains(preview) {
        String::new()
    } else {
        preview.to_string()
    }
}

/// Linear merge of two sorted, deduplicated slot lists.
fn intersect_sorted(left: &[Slot], right: &[Slot]) -> Vec<Slot> {
    let mut result = Vec::with_capacity(left.len().min(right.len()));
    let (mut i, mut j) = (0, 0);
    while i < left.len() && j < right.len() {
        match left[i].cmp(&right[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                result.push(left[i]);
                i += 1;
                j += 1;
            }
        }
    }
    result
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
            SearchDocument::new("Release confirmation 4J7K", "", None, None, None),
        );
        state.insert(
            "two".to_string(),
            SearchDocument::new("unrelated", "", None, None, None),
        );

        assert_eq!(
            state.matches("CONFIRMATION 4j7"),
            HashSet::from(["one".to_string()])
        );
    }

    /// The index must return exactly what a brute-force scan would.
    ///
    /// Trigram filtering is an optimisation over "check every document", so any
    /// disagreement is a bug -- a missing hit means a clip the user cannot find.
    #[test]
    fn index_agrees_with_a_brute_force_scan() {
        let corpus = [
            ("a", "Release confirmation 4J7K for the Wilmore office"),
            ("b", "release notes: clipboard history and paste engine"),
            ("c", "unrelated content about gardening"),
            ("d", "CONFIRMATION of receipt, reference 4J7K"),
            ("e", "café · Ελληνικά · 日本語 · emoji 😀 tail"),
            ("f", ""),
        ];

        let mut state = IndexState::default();
        for (id, text) in corpus {
            state.insert(
                id.to_string(),
                SearchDocument::new(text, "", None, None, None),
            );
        }

        for query in [
            "release",
            "RELEASE",
            "4j7k",
            "confirmation",
            "clipboard history",
            "日本語",
            "café",
            "😀",
            "gardening",
            "nothing matches this",
            "ab",
            "",
            "  ",
        ] {
            let normalized = normalize(query);
            let expected: HashSet<String> = corpus
                .iter()
                .filter(|(_, text)| normalize(text).contains(&normalized))
                .map(|(id, _)| (*id).to_string())
                .collect();
            assert_eq!(
                state.matches(query),
                expected,
                "index and scan disagree for {query:?}"
            );
        }
    }

    /// Slots are reused after a removal, so a stale posting would make a new
    /// clip answer the deleted clip's queries.
    #[test]
    fn a_reused_slot_does_not_inherit_the_removed_clips_matches() {
        let mut state = IndexState::default();
        state.insert(
            "first".to_string(),
            SearchDocument::new("distinctive alpha payload", "", None, None, None),
        );
        assert!(state.matches("distinctive").contains("first"));

        state.remove("first");
        assert!(state.matches("distinctive").is_empty());
        assert_eq!(state.free_slots.len(), 1, "the slot should be reusable");

        state.insert(
            "second".to_string(),
            SearchDocument::new("completely separate bravo text", "", None, None, None),
        );
        assert!(state.matches("bravo").contains("second"));
        assert!(
            state.matches("distinctive").is_empty(),
            "the reused slot answered the removed clip's query"
        );
        assert_eq!(state.len(), 1);
    }

    /// Removing every document must leave no postings behind, or the index
    /// grows monotonically across a long session of captures and deletions.
    #[test]
    fn postings_are_emptied_when_documents_go_away() {
        let mut state = IndexState::default();
        for index in 0..8 {
            state.insert(
                format!("id{index}"),
                SearchDocument::new(&format!("payload number {index}"), "", None, None, None),
            );
        }
        assert!(!state.postings.is_empty());

        for index in 0..8 {
            state.remove(&format!("id{index}"));
        }
        assert!(state.postings.is_empty(), "postings leaked after removal");
        assert_eq!(state.len(), 0);
    }

    #[test]
    fn a_preview_the_content_already_holds_is_not_stored_twice() {
        // Text clip: the preview is a truncation of the content.
        let document = SearchDocument::new(
            "the full clip text goes here",
            "the full clip",
            None,
            None,
            None,
        );
        assert!(document.preview.is_empty(), "redundant preview was stored");
        assert!(
            document.contains("the full clip"),
            "still searchable via content"
        );

        // Image clip: content is empty, so the preview is the only text there
        // is and must be kept.
        let image = SearchDocument::new("", "Screenshot of the invoice", None, None, None);
        assert_eq!(image.preview, "screenshot of the invoice");
        assert!(image.contains("invoice"));

        // A preview that is not a clean substring is kept as-is.
        let ellipsis =
            SearchDocument::new("some longer body text", "some longer…", None, None, None);
        assert_eq!(ellipsis.preview, "some longer…");
    }

    /// Trimming a large history must give the memory back.
    ///
    /// Retention prunes the oldest clips, which hold the lowest slots, so this
    /// deletes from the front -- the case tail-truncation would miss.
    #[test]
    fn slots_are_reclaimed_after_a_large_history_is_trimmed() {
        let mut state = IndexState::default();
        for index in 0..1_000 {
            state.insert(
                format!("id{index}"),
                SearchDocument::new(&format!("clip body number {index}"), "", None, None, None),
            );
        }
        assert_eq!(state.documents.len(), 1_000);

        // Delete the oldest 990, the way retention would.
        for index in 0..990 {
            state.remove(&format!("id{index}"));
        }

        assert_eq!(state.len(), 10, "ten clips should remain");
        assert!(
            state.documents.len() <= 32,
            "slots were not reclaimed: {} slots for 10 clips",
            state.documents.len()
        );
        assert!(state.free_slots.len() <= 32);

        // The survivors are still findable, and the deleted ones are gone.
        assert!(state.matches("number 995").contains("id995"));
        assert!(state.matches("number 100").is_empty());

        // Postings must not reference a slot that no longer exists.
        for slots in state.postings.values() {
            for slot in slots {
                assert!(
                    (*slot as usize) < state.documents.len()
                        && state.documents[*slot as usize].is_some(),
                    "posting points at a dead slot"
                );
            }
            let mut sorted = slots.clone();
            sorted.sort_unstable();
            assert_eq!(*slots, sorted, "compaction must preserve sorted postings");
        }
    }

    /// Compaction rewrites every slot, so the id -> slot map has to move with
    /// it or lookups silently return the wrong clip.
    #[test]
    fn compaction_keeps_id_lookups_pointing_at_the_right_document() {
        let mut state = IndexState::default();
        for index in 0..600 {
            state.insert(
                format!("id{index}"),
                SearchDocument::new(&format!("unique marker {index} here"), "", None, None, None),
            );
        }
        for index in 0..500 {
            state.remove(&format!("id{index}"));
        }

        for index in 500..600 {
            let id = format!("id{index}");
            let document = state
                .document(&id)
                .expect("survivor should still be indexed");
            assert!(
                document.contains(&format!("unique marker {index}")),
                "id {id} resolved to the wrong document after compaction"
            );
        }
    }

    #[test]
    fn sorted_intersection_keeps_only_common_slots() {
        assert_eq!(intersect_sorted(&[1, 3, 5, 7], &[3, 4, 5, 9]), vec![3, 5]);
        assert_eq!(intersect_sorted(&[], &[1, 2]), Vec::<Slot>::new());
        assert_eq!(intersect_sorted(&[1, 2], &[]), Vec::<Slot>::new());
        assert_eq!(intersect_sorted(&[1, 2, 3], &[1, 2, 3]), vec![1, 2, 3]);
        assert_eq!(intersect_sorted(&[1, 2], &[3, 4]), Vec::<Slot>::new());
    }

    #[test]
    fn postings_stay_sorted_and_deduplicated_across_reinserts() {
        let mut state = IndexState::default();
        for index in 0..5 {
            state.insert(
                format!("id{index}"),
                SearchDocument::new("shared trigram body", "", None, None, None),
            );
        }
        // Re-insert an existing id: the slot is reused, not duplicated.
        state.insert(
            "id2".to_string(),
            SearchDocument::new("shared trigram body", "", None, None, None),
        );

        for slots in state.postings.values() {
            let mut sorted = slots.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(*slots, sorted, "postings must stay sorted and unique");
        }
        assert_eq!(state.matches("shared trigram").len(), 5);
    }

    #[test]
    fn supports_short_queries_and_ocr_updates() {
        let index = SearchIndex::default();
        *index.state.write() = Some(IndexState::default());
        index.upsert("image", "image", &[], "Screenshot", None, None);
        assert!(index.matches("sc").contains("image"));

        index.update_ocr("image", "The clipboard service is unavailable");
        assert!(index.matches("service is unavailable").contains("image"));
        index.upsert("image", "image", &[], "Screenshot", None, None);
        assert!(index.matches("service is unavailable").contains("image"));
        index.remove("image");
        assert!(index.matches("service").is_empty());
    }
}
