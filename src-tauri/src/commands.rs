use crate::database::Database;
pub(crate) use crate::managed_image::remove_clip_image_files;
use crate::models::{Clip, ClipboardItem, Folder, FolderItem, OcrHighlights, OcrMatch, OcrRect};
use clipboard_rs::common::RustImage;
use clipboard_rs::{Clipboard, ClipboardContent, ClipboardContext, RustImageData};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager};

fn clip_to_list_item(clip: &Clip, preview_only: bool) -> ClipboardItem {
    // A hidden clip ships no content and no preview — not even a thumbnail.
    // Blanking it in the frontend instead would leave the secret sitting in the
    // renderer's memory and in every IPC payload, which makes "hidden" a
    // decoration rather than a property. Revealing for the session fetches the
    // real payload on demand through get_clip_details.
    //
    // preview_only is what the flyout and History window already request.
    // ClipCard only renders the truncated preview (and the image thumbnail),
    // and paste/copy go by uuid, so the full decrypted body does not belong
    // on a text list row. Image rows still ship the thumbnail in `content`
    // because that is what imageSrcFromContent displays.
    let hidden = clip.is_hidden;
    ClipboardItem {
        id: clip.uuid.clone(),
        clip_type: clip.clip_type.clone(),
        content: crate::clip_list::list_item_content(
            &clip.clip_type,
            &clip.content,
            preview_only,
            hidden,
        ),
        preview: crate::clip_list::list_item_preview(&clip.text_preview, hidden),
        folder_id: clip.folder_id.map(|id| id.to_string()),
        is_pinned: clip.is_pinned,
        created_at: clip.created_at.to_rfc3339(),
        source_app: clip.source_app.clone(),
        source_icon: clip.source_icon.clone(),
        metadata: clip.metadata.clone(),
        has_ocr_text: clip
            .ocr_text
            .as_deref()
            .is_some_and(|text| !text.trim().is_empty()),
        // Search snippets and highlight boxes are made of the very text
        // being hidden, so they have to go too.
        ocr_match: None,
        ocr_highlights: None,
        image_expired: clip.full_image_expired,
        // The note goes too on a hidden row. It is text the user wrote *about*
        // this clip — "AWS root password" is a plausible note on exactly the
        // kind of clip worth hiding — so shipping it would put the secret back
        // on the row by another route. Revealing fetches it with the payload.
        notes: crate::clip_list::list_item_notes(clip.notes.as_deref(), hidden),
        is_hidden: hidden,
    }
}

/// The selectable word layout for an image clip, as fractions of the image.
/// Returns None when there is nothing usable to select.
fn ocr_text_layout(ocr_words_json: &str) -> Option<crate::models::OcrTextLayout> {
    let layout: crate::ocr::OcrLayout = serde_json::from_str(ocr_words_json).ok()?;
    if layout.image_width == 0 || layout.image_height == 0 || layout.words.is_empty() {
        return None;
    }
    let width = layout.image_width as f32;
    let height = layout.image_height as f32;

    // Recorded indices are the engine's own line numbers; fall back to inferred
    // bands for layouts stored before those were kept.
    let inferred = crate::ocr::infer_line_indices(&layout.words);
    let recorded: Vec<u32> = layout
        .words
        .iter()
        .zip(inferred.iter())
        .map(|(word, fallback)| word.line.unwrap_or(*fallback))
        .collect();

    // Densify: the engine skips empty words, so its indices can have gaps, and
    // the UI wants "line N+1 follows line N" to mean exactly one break.
    let mut dense = Vec::with_capacity(recorded.len());
    let mut previous: Option<u32> = None;
    let mut next_line = 0u32;
    for raw in recorded {
        match previous {
            Some(last) if last == raw => {}
            Some(_) => next_line += 1,
            None => {}
        }
        previous = Some(raw);
        dense.push(next_line);
    }

    let words = layout
        .words
        .iter()
        .zip(dense)
        .map(|(word, line)| crate::models::OcrTextWord {
            text: word.text.clone(),
            x: word.x / width,
            y: word.y / height,
            width: word.width / width,
            height: word.height / height,
            line,
        })
        .collect();

    Some(crate::models::OcrTextLayout {
        aspect: width / height,
        words,
    })
}

/// Build the highlight overlay for an image search result: the word boxes whose
/// text matches the query, expressed as fractions of the image plus its aspect
/// ratio (SOU-242 phase 2). Returns None when nothing usable matches.
fn build_ocr_highlights(ocr_words_json: &str, query: &str) -> Option<OcrHighlights> {
    let tokens: Vec<String> = query
        .split_whitespace()
        .filter(|token| token.chars().count() >= 2)
        .map(|token| token.to_lowercase())
        .collect();
    if tokens.is_empty() {
        return None;
    }

    let layout: crate::ocr::OcrLayout = serde_json::from_str(ocr_words_json).ok()?;
    if layout.image_width == 0 || layout.image_height == 0 {
        return None;
    }
    let width = layout.image_width as f32;
    let height = layout.image_height as f32;

    let boxes: Vec<OcrRect> = layout
        .words
        .iter()
        .filter(|word| {
            let lowered = word.text.to_lowercase();
            tokens.iter().any(|token| lowered.contains(token))
        })
        .map(|word| OcrRect {
            x: (word.x / width).clamp(0.0, 1.0),
            y: (word.y / height).clamp(0.0, 1.0),
            width: (word.width / width).clamp(0.0, 1.0),
            height: (word.height / height).clamp(0.0, 1.0),
        })
        .collect();

    if boxes.is_empty() {
        return None;
    }
    Some(OcrHighlights {
        aspect: width / height,
        boxes,
    })
}

const OCR_SNIPPET_CHAR_LIMIT: usize = 96;

/// Cap on a clip note, mirroring `NOTE_CHAR_LIMIT` in the frontend constants.
/// A note is a short reminder of what a clip is, so this is generous for that
/// job while keeping an accidental paste of a whole document out of the
/// encrypted column and the trigram index.
const NOTE_CHAR_LIMIT: usize = 500;

/// Returned when a clip's full-resolution image was dropped by retention
/// (SOU-244). Surfaced to the user when they try to paste/copy the full image.
pub(crate) const IMAGE_EXPIRED_ERROR: &str =
    "This screenshot's full image has expired. Only its recognized text remains.";

fn find_case_insensitive(haystack: &str, needle: &str) -> Option<(usize, usize)> {
    let folded_needle = needle.to_lowercase();
    if folded_needle.is_empty() {
        return None;
    }

    for (start, _) in haystack.char_indices() {
        let mut folded_candidate = String::new();
        for (relative_start, character) in haystack[start..].char_indices() {
            folded_candidate.extend(character.to_lowercase());
            if !folded_needle.starts_with(&folded_candidate) {
                break;
            }
            if folded_candidate == folded_needle {
                return Some((start, start + relative_start + character.len_utf8()));
            }
        }
    }

    None
}

fn normalize_ocr_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn tail_chars(value: &str, limit: usize) -> (String, bool) {
    let characters: Vec<char> = value.chars().collect();
    if characters.len() <= limit {
        return (value.to_string(), false);
    }

    let mut tail: String = characters[characters.len() - limit..].iter().collect();
    if let Some(first_space) = tail.find(char::is_whitespace) {
        tail = tail[first_space..].trim_start().to_string();
    }
    (tail, true)
}

fn head_chars(value: &str, limit: usize) -> (String, bool) {
    let characters: Vec<char> = value.chars().collect();
    if characters.len() <= limit {
        return (value.to_string(), false);
    }

    let mut head: String = characters[..limit].iter().collect();
    if let Some(last_space) = head.rfind(char::is_whitespace) {
        head.truncate(last_space);
    }
    (head, true)
}

fn build_ocr_match(ocr_text: &str, query: &str) -> Option<OcrMatch> {
    let query = query.trim();
    let (match_start, match_end) = find_case_insensitive(ocr_text, query)?;
    let matched = normalize_ocr_whitespace(&ocr_text[match_start..match_end]);
    let before = normalize_ocr_whitespace(&ocr_text[..match_start]);
    let after = normalize_ocr_whitespace(&ocr_text[match_end..]);

    let matched_length = matched.chars().count();
    if matched_length >= OCR_SNIPPET_CHAR_LIMIT {
        let (mut matched, cropped) = head_chars(&matched, OCR_SNIPPET_CHAR_LIMIT - 1);
        if cropped {
            matched.push('…');
        }
        return Some(OcrMatch {
            before: String::new(),
            matched,
            after: String::new(),
        });
    }

    // Reserve the maximum decoration cost: one separator and one ellipsis on
    // each side. Very long matches remain useful on their own without context.
    if matched_length + 4 >= OCR_SNIPPET_CHAR_LIMIT {
        return Some(OcrMatch {
            before: String::new(),
            matched,
            after: String::new(),
        });
    }

    let remaining = OCR_SNIPPET_CHAR_LIMIT - matched_length - 4;
    let before_limit = remaining / 2;
    let after_limit = remaining - before_limit;
    let (mut before, before_cropped) = tail_chars(&before, before_limit);
    let (mut after, after_cropped) = head_chars(&after, after_limit);

    if before_cropped {
        before = format!("…{before}");
    }
    if !before.is_empty() {
        before.push(' ');
    }
    if !after.is_empty() {
        after.insert(0, ' ');
    }
    if after_cropped {
        after.push('…');
    }

    Some(OcrMatch {
        before,
        matched,
        after,
    })
}

fn clip_to_search_item(clip: &Clip, query: &str, preview_only: bool) -> ClipboardItem {
    // Search is a list UI path (flyout and History). SBS-829 fixed get_clips;
    // SBS-912 applies the same mapping here so a keystroke does not ship the
    // decrypted body. OCR snippets and highlight boxes still attach below
    // for a visible image hit; hidden rows return before that.
    let mut item = clip_to_list_item(clip, preview_only);
    // Hidden list rows already blank content, notes, and OCR snippets.
    // Search must not write those fields back from the decrypted image OCR.
    if clip.is_hidden {
        return item;
    }
    if clip.clip_type == "image" {
        item.ocr_match = clip
            .ocr_text
            .as_deref()
            .and_then(|text| build_ocr_match(text, query));
        item.ocr_highlights = clip
            .ocr_words
            .as_deref()
            .and_then(|words| build_ocr_highlights(words, query));
    }
    item
}

fn decrypt_clip_fields(db: &Database, clip: &mut Clip) -> Result<(), String> {
    // Content and preview are the clip. Everything below is decoration, and a
    // broken decoration must not make an otherwise readable clip disappear —
    // the search index already treats the source app that way
    // (`SearchIndex::ensure_ready`), so listing and details now match it.
    clip.content = db.crypto.decrypt(&clip.content)?;
    clip.text_preview = db.crypto.decrypt_text(&clip.text_preview)?;
    if let Err(error) = db.crypto.decrypt_optional_text(&mut clip.source_app) {
        log::warn!(
            "CLIPS: Ignoring an unreadable source app on clip {}: {error}",
            clip.uuid
        );
        clip.source_app = None;
    }
    if let Err(error) = db.crypto.decrypt_optional_text(&mut clip.source_icon) {
        log::warn!(
            "CLIPS: Ignoring an unreadable source icon on clip {}: {error}",
            clip.uuid
        );
        clip.source_icon = None;
    }
    if let Err(error) = db.crypto.decrypt_optional_text(&mut clip.metadata) {
        log::warn!(
            "CLIPS: Ignoring unreadable metadata on clip {}: {error}",
            clip.uuid
        );
        clip.metadata = None;
    }
    // OCR text is auxiliary; never let a bad value block loading the clip.
    if db.crypto.decrypt_optional_text(&mut clip.ocr_text).is_err() {
        clip.ocr_text = None;
    }
    // OCR word boxes are likewise auxiliary (highlighting only).
    if db
        .crypto
        .decrypt_optional_text(&mut clip.ocr_words)
        .is_err()
    {
        clip.ocr_words = None;
    }
    // A note is auxiliary too: an unreadable one must not stop the clip loading.
    // Logged rather than silently dropped — the note vanishes from the UI either
    // way, and the log is the only signal that something was there to lose.
    // Matches how the search index reports the same failure.
    if let Err(error) = db.crypto.decrypt_optional_text(&mut clip.notes) {
        log::warn!("CLIPS: Ignoring an unreadable note: {error}");
        clip.notes = None;
    }
    Ok(())
}

/// Decrypt a list/search row. One unreadable neighbor must not fail the
/// whole page: skip it and keep the rest. Single-clip reads still go
/// through `decrypt_clip_fields` so they surface the error for that clip.
fn decrypt_listed_clip(db: &Database, clip: &mut Clip) -> bool {
    match decrypt_clip_fields(db, clip) {
        Ok(()) => true,
        Err(error) => {
            log::warn!("CLIPS: Ignoring unreadable clip {}: {error}", clip.uuid);
            false
        }
    }
}

/// One batch of ordered rows, plus whether the source has anything left after
/// it. The flag is the source's own business: a SQL page is exhausted when it
/// comes back short, while an id list is exhausted when the ids run out, which
/// is not the same thing once a row is deleted mid-scan.
struct ClipBatch {
    rows: Vec<Clip>,
    exhausted: bool,
}

/// Where in an already-ordered source to start a page.
///
/// `after_id` is the last row the client already has. Starting just after it
/// is exact even when an earlier page skipped unreadable neighbors. A bare
/// `offset` is a source-row index (SQL OFFSET / id-list index), which matches
/// the displayed count when every earlier row decrypted. SBS-993: never walk
/// and decrypt `[0, offset)` to count readable rows.
fn listing_cursor_id(after_id: Option<&str>) -> Option<&str> {
    after_id.map(str::trim).filter(|id| !id.is_empty())
}

fn source_start_for_ids(ids: &[String], after_id: Option<&str>, offset: usize) -> usize {
    match listing_cursor_id(after_id) {
        Some(id) => match ids.iter().position(|candidate| candidate == id) {
            Some(index) => index.saturating_add(1),
            None => {
                // Unknown is not "start over": restarting would re-decrypt and
                // re-send the prefix the client already displayed. Fall back to
                // the source offset the client sent alongside the cursor.
                log::warn!(
                    "CLIPS: listing cursor {id} is not in this result set; falling back to offset {offset}"
                );
                offset
            }
        },
        None => offset,
    }
}

/// Collect `limit` readable rows starting at `source_start` in the ordered
/// source. Unreadable neighbors are skipped and the walk continues forward so
/// a short page still means "no more history" (SBS-830). The walk never
/// rewinds before `source_start`, so a later page does not re-decrypt the
/// prefix (SBS-993).
async fn collect_readable_clips<F, Fut>(
    db: &Database,
    source_start: usize,
    limit: usize,
    fetch_batch: F,
) -> Result<Vec<Clip>, String>
where
    F: Fn(usize, usize) -> Fut,
    Fut: std::future::Future<Output = Result<ClipBatch, String>>,
{
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut page: Vec<Clip> = Vec::new();
    let mut scanned = source_start;
    loop {
        let batch = fetch_batch(scanned, limit).await?;
        scanned = scanned.saturating_add(limit);
        for mut clip in batch.rows {
            if !decrypt_listed_clip(db, &mut clip) {
                continue;
            }
            page.push(clip);
            if page.len() == limit {
                return Ok(page);
            }
        }
        if batch.exhausted {
            return Ok(page);
        }
    }
}

/// Readable-row paging over an id list the caller has already ordered and
/// filtered (search hits, source-app matches). `source_start` is an index
/// into `ids`, not a readable-row count.
async fn collect_readable_clips_by_id(
    db: &Database,
    ids: &[String],
    source_start: usize,
    limit: usize,
) -> Result<Vec<Clip>, String> {
    collect_readable_clips(db, source_start, limit, |start, count| async move {
        let end = start.saturating_add(count).min(ids.len());
        let chunk = ids.get(start..end).unwrap_or_default();
        Ok(ClipBatch {
            rows: fetch_clips_by_id(&db.pool, chunk).await?,
            exhausted: end >= ids.len(),
        })
    })
    .await
}

async fn cleanup_orphan_clip_image_files(
    pool: &SqlitePool,
    image_dir: &std::path::Path,
) -> Result<(), String> {
    let orphan_paths: Vec<Option<String>> = sqlx::query_scalar(
        r#"
        SELECT file_path
        FROM clip_images
        WHERE clip_uuid NOT IN (SELECT uuid FROM clips)
        "#,
    )
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;

    sqlx::query(r#"DELETE FROM clip_images WHERE clip_uuid NOT IN (SELECT uuid FROM clips)"#)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;

    remove_clip_image_files(image_dir, orphan_paths.into_iter().flatten().collect());

    Ok(())
}

fn encrypt_existing_text(
    crypto: &crate::crypto::CryptoManager,
    value: &str,
) -> Result<String, String> {
    if crypto.is_encrypted_text(value) {
        Ok(value.to_string())
    } else {
        crypto.encrypt_text(value)
    }
}

fn encrypt_existing_optional_text(
    crypto: &crate::crypto::CryptoManager,
    value: Option<&str>,
) -> Result<Option<String>, String> {
    value
        .map(|value| encrypt_existing_text(crypto, value))
        .transpose()
}

async fn image_bytes_for_encryption_migration(
    db: &Database,
    clip: &Clip,
) -> Result<(Vec<u8>, Option<String>), String> {
    let row: Option<(Option<String>, Vec<u8>)> =
        sqlx::query_as("SELECT file_path, full_content FROM clip_images WHERE clip_uuid = ?")
            .bind(&clip.uuid)
            .fetch_optional(&db.pool)
            .await
            .map_err(|e| e.to_string())?;

    if let Some((file_path, full_content)) = row {
        if let Some(path) = file_path.as_deref().filter(|path| !path.is_empty()) {
            if let Ok(stored) = std::fs::read(path) {
                let plaintext = if db.crypto.is_encrypted(&stored) {
                    db.crypto.decrypt(&stored)?
                } else {
                    stored
                };
                return Ok((plaintext, file_path));
            }
        }
        if !full_content.is_empty() {
            let plaintext = if db.crypto.is_encrypted(&full_content) {
                db.crypto.decrypt(&full_content)?
            } else {
                full_content
            };
            return Ok((plaintext, file_path));
        }
    }

    if !clip.content.is_empty() && !db.crypto.is_encrypted(&clip.content) {
        return Ok((clip.content.clone(), None));
    }
    Err(format!("image payload is missing for clip {}", clip.uuid))
}

pub async fn migrate_encrypted_storage(db: &Database) -> Result<u64, String> {
    let version: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'storage_encryption_version'")
            .fetch_optional(&db.pool)
            .await
            .map_err(|e| e.to_string())?;
    if version.as_deref() == Some("1") {
        return Ok(0);
    }

    let clips: Vec<Clip> = sqlx::query_as("SELECT * FROM clips ORDER BY id")
        .fetch_all(&db.pool)
        .await
        .map_err(|e| e.to_string())?;
    let mut migrated = 0_u64;

    for clip in clips {
        let (plaintext, new_image_path, old_image_path) = if clip.clip_type == "image" {
            let (full_image, old_path) = image_bytes_for_encryption_migration(db, &clip).await?;
            let preview = crate::clipboard::create_image_preview(&full_image)?;
            let new_path = crate::clipboard::persist_full_image_file(
                &db.crypto,
                &db.image_dir,
                &clip.uuid,
                &full_image,
            )?;
            (preview, Some((new_path, full_image)), old_path)
        } else {
            let plaintext = if db.crypto.is_encrypted(&clip.content) {
                db.crypto.decrypt(&clip.content)?
            } else {
                clip.content.clone()
            };
            (plaintext, None, None)
        };

        let hash_source = new_image_path
            .as_ref()
            .map(|(_, full_image)| full_image.as_slice())
            .unwrap_or(plaintext.as_slice());
        let encrypted_content = db.crypto.encrypt(&plaintext)?;
        let encrypted_preview = encrypt_existing_text(&db.crypto, &clip.text_preview)?;
        let encrypted_source_app =
            encrypt_existing_optional_text(&db.crypto, clip.source_app.as_deref())?;
        let encrypted_source_icon =
            encrypt_existing_optional_text(&db.crypto, clip.source_icon.as_deref())?;
        let encrypted_metadata =
            encrypt_existing_optional_text(&db.crypto, clip.metadata.as_deref())?;

        let mut transaction = db.pool.begin().await.map_err(|e| e.to_string())?;
        sqlx::query(
            r#"
            UPDATE clips
            SET content = ?, text_preview = ?, content_hash = ?, source_app = ?, source_icon = ?, metadata = ?, is_thumbnail = ?
            WHERE uuid = ?
            "#,
        )
        .bind(encrypted_content)
        .bind(encrypted_preview)
        .bind(db.crypto.keyed_hash(hash_source))
        .bind(encrypted_source_app)
        .bind(encrypted_source_icon)
        .bind(encrypted_metadata)
        .bind(clip.clip_type == "image")
        .bind(&clip.uuid)
        .execute(&mut *transaction)
        .await
        .map_err(|e| e.to_string())?;

        if let Some((path, full_image)) = &new_image_path {
            sqlx::query(
                r#"
                INSERT OR REPLACE INTO clip_images
                    (clip_uuid, full_content, file_path, file_size, storage_kind, mime_type, created_at)
                VALUES (?, x'', ?, ?, 'encrypted_file', 'image/png', CURRENT_TIMESTAMP)
                "#,
            )
            .bind(&clip.uuid)
            .bind(path)
            .bind(full_image.len() as i64)
            .execute(&mut *transaction)
            .await
            .map_err(|e| e.to_string())?;
        }
        transaction.commit().await.map_err(|e| e.to_string())?;

        if let (Some(old_path), Some((new_path, _))) = (old_image_path, &new_image_path) {
            if old_path != *new_path {
                remove_clip_image_files(&db.image_dir, vec![old_path]);
            }
        }
        migrated += 1;
    }

    sqlx::query(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('storage_encryption_version', '1')",
    )
    .execute(&db.pool)
    .await
    .map_err(|e| e.to_string())?;
    Ok(migrated)
}

pub async fn migrate_clip_format_model(db: &Database) -> Result<u64, String> {
    let version: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'clip_format_model_version'")
            .fetch_optional(&db.pool)
            .await
            .map_err(|e| e.to_string())?;
    if version.as_deref() == Some("1") {
        return Ok(0);
    }

    let mut clips: Vec<Clip> = sqlx::query_as("SELECT * FROM clips ORDER BY id")
        .fetch_all(&db.pool)
        .await
        .map_err(|e| e.to_string())?;
    for clip in &mut clips {
        decrypt_clip_fields(db, clip)?;
        let formats = if clip.clip_type == "image" {
            Vec::new()
        } else {
            load_clip_formats(db, &clip.uuid).await?
        };
        let full_image = if clip.clip_type == "image" {
            Some(load_full_image_content(db, clip).await?)
        } else {
            None
        };
        let hash_material = crate::clipboard::build_clip_hash_material(
            &clip.clip_type,
            full_image.as_deref().unwrap_or(&clip.content),
            formats
                .iter()
                .map(|(format, content)| (format.as_str(), content.as_slice())),
        );
        sqlx::query("UPDATE clips SET content_hash = ? WHERE uuid = ?")
            .bind(db.crypto.keyed_hash(&hash_material))
            .bind(&clip.uuid)
            .execute(&db.pool)
            .await
            .map_err(|e| e.to_string())?;
    }
    sqlx::query(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('clip_format_model_version', '1')",
    )
    .execute(&db.pool)
    .await
    .map_err(|e| e.to_string())?;
    Ok(clips.len() as u64)
}

pub(crate) async fn load_full_image_content(
    db: &Database,
    clip: &mut Clip,
) -> Result<Vec<u8>, String> {
    let pool = &db.pool;
    if clip.clip_type != "image" {
        return Err("Clip is not an image".to_string());
    }

    // Retention dropped the full-resolution blob (SOU-244); only the thumbnail
    // and OCR text remain. Refuse rather than silently handing back the low-res
    // thumbnail as if it were the original. The frontend also gates this, so
    // this is the safety net for any direct paste/copy path.
    if clip.full_image_expired {
        return Err(IMAGE_EXPIRED_ERROR.to_string());
    }

    // 1. Try fetching from file path in DB
    let file_path: Option<String> =
        sqlx::query_scalar(r#"SELECT file_path FROM clip_images WHERE clip_uuid = ?"#)
            .bind(&clip.uuid)
            .fetch_optional(pool)
            .await
            .map_err(|e| e.to_string())?;

    if let Some(path) = file_path {
        if !path.is_empty() {
            // If file exists, return it
            if let Ok(bytes) = crate::clipboard::read_full_image_file(&db.crypto, &path) {
                return Ok(bytes);
            }
            // If file missing, try fallbacks below
            log::warn!("Stored image file is missing; checking database fallbacks");
        }
    }

    // 2. Try DB blob (migration not done or failed)
    let full_content: Option<Vec<u8>> =
        sqlx::query_scalar(r#"SELECT full_content FROM clip_images WHERE clip_uuid = ?"#)
            .bind(&clip.uuid)
            .fetch_optional(pool)
            .await
            .map_err(|e| e.to_string())?;

    if let Some(content) = full_content {
        if !content.is_empty() {
            return if db.crypto.is_encrypted(&content) {
                db.crypto.decrypt(&content)
            } else {
                Ok(content)
            };
        }
    }

    // 3. Legacy content in clips table
    if !clip.content.is_empty() {
        return if db.crypto.is_encrypted(&clip.content) {
            db.crypto.decrypt(&clip.content)
        } else {
            Ok(clip.content.clone())
        };
    }

    Err("Image content missing".to_string())
}

async fn load_clip_formats(
    db: &Database,
    clip_uuid: &str,
) -> Result<Vec<(String, Vec<u8>)>, String> {
    let rows: Vec<(String, Vec<u8>)> = sqlx::query_as(
        "SELECT format, content FROM clip_formats WHERE clip_uuid = ? ORDER BY format",
    )
    .bind(clip_uuid)
    .fetch_all(&db.pool)
    .await
    .map_err(|e| e.to_string())?;
    rows.into_iter()
        .map(|(format, encrypted)| Ok((format, db.crypto.decrypt(&encrypted)?)))
        .collect()
}

fn clipboard_contents_for_restore(
    clip: &Clip,
    full_image: Option<&[u8]>,
    formats: &[(String, Vec<u8>)],
    plain_text: bool,
) -> Result<Vec<ClipboardContent>, String> {
    let plain_content = String::from_utf8_lossy(&clip.content).to_string();
    let restoring_image = full_image.is_some();
    let mut contents = if let Some(image) = full_image {
        vec![ClipboardContent::Image(
            RustImageData::from_bytes(image).map_err(|e| e.to_string())?,
        )]
    } else {
        vec![ClipboardContent::Text(plain_content)]
    };
    // Image clips restore only their durable bitmap. Adding auxiliary rich
    // formats can change how a paste target classifies the restored payload;
    // HTML and RTF are therefore replayed only for text clips.
    if !plain_text && !restoring_image {
        for (format, content) in formats {
            match format.as_str() {
                // Stored HTML is the header-stripped document (see cf_html.rs);
                // clipboard-rs's multi-content set() writes it raw, so re-attach
                // a valid CF_HTML header or Office-class apps reject the paste.
                "html" => contents.push(ClipboardContent::Html(crate::cf_html::to_cf_html(
                    &String::from_utf8(content.clone())
                        .map_err(|_| "stored HTML is not UTF-8".to_string())?,
                ))),
                "rtf" => contents.push(ClipboardContent::Rtf(
                    String::from_utf8(content.clone())
                        .map_err(|_| "stored RTF is not UTF-8".to_string())?,
                )),
                _ => {}
            }
        }
    }
    Ok(contents)
}

/// SQL restriction for the flyout's content tabs. Filtering must happen in the
/// query — the list pages 20 rows at a time, and filtering a page client-side
/// hides matching items that live on unfetched pages.
fn content_filter_clause(content_filter: Option<&str>) -> &'static str {
    match content_filter {
        Some("images") => " AND clip_type = 'image'",
        Some("text") => " AND clip_type = 'text'",
        _ => "",
    }
}

/// The History window's date-range filter (SOU-585). `created_at` is stored in
/// the clear, unlike the app name, so SQL can apply this one directly and
/// paging stays authoritative in the database.
///
/// The range is half-open, `[from, to)`. The caller passes the instant after
/// the last one it wants, so a "Today" preset is `[midnight, tomorrow)` and
/// nothing has to reason about the last representable moment of a day.
///
/// Returns the SQL fragment; bind `from` then `to` (whichever are present)
/// immediately after the folder id and before limit/offset.
fn date_range_clause(from: Option<&str>, to: Option<&str>) -> String {
    let mut clause = String::new();
    if from.is_some() {
        clause.push_str(" AND created_at >= ?");
    }
    if to.is_some() {
        clause.push_str(" AND created_at < ?");
    }
    clause
}

/// Load a page of clips by id, still ordered the way the database ordered them.
/// Used by every path that has to narrow ids outside SQL (search, source app)
/// before fetching the encrypted payloads for just the rows being shown.
///
/// Every clip listing orders by `is_pinned DESC, created_at DESC, uuid DESC`.
/// The `uuid` key is what makes that ordering *total*: `created_at` has
/// one-second resolution, so a burst of copies ties, and SQLite is free to
/// break a tie differently between two executions of the same query. The
/// paging paths order once to pick a page and then order again here to fetch
/// it, so without a deterministic tie-break the second ordering could disagree
/// with the first and a row could be repeated on one page and missing from the
/// next.
async fn fetch_clips_by_id(pool: &SqlitePool, ids: &[String]) -> Result<Vec<Clip>, String> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT * FROM clips WHERE is_deleted = 0 AND uuid IN ({placeholders}) \
         ORDER BY is_pinned DESC, created_at DESC, uuid DESC"
    );
    let mut query = sqlx::query_as::<_, Clip>(&sql);
    for id in ids {
        query = query.bind(id);
    }
    query.fetch_all(pool).await.map_err(|e| e.to_string())
}

/// Filters that SQL can apply itself, assembled into one WHERE body so every
/// listing path restricts identically.
fn clip_where_body(
    folder_id: Option<i64>,
    content_filter: Option<&str>,
    from: Option<&str>,
    to: Option<&str>,
) -> String {
    let mut sql = String::from("is_deleted = 0");
    if folder_id.is_some() {
        sql.push_str(" AND folder_id = ?");
    }
    sql.push_str(content_filter_clause(content_filter));
    sql.push_str(&date_range_clause(from, to));
    sql
}

async fn fetch_ordered_clip_ids(
    pool: &SqlitePool,
    where_body: &str,
    folder_id: Option<i64>,
    date_from: Option<&str>,
    date_to: Option<&str>,
) -> Result<Vec<String>, String> {
    let sql = format!(
        "SELECT uuid FROM clips WHERE {where_body} ORDER BY is_pinned DESC, created_at DESC, uuid DESC"
    );
    let mut query = sqlx::query_scalar::<_, String>(&sql);
    if let Some(id) = folder_id {
        query = query.bind(id);
    }
    if let Some(from) = date_from {
        query = query.bind(from.to_string());
    }
    if let Some(to) = date_to {
        query = query.bind(to.to_string());
    }
    query.fetch_all(pool).await.map_err(|e| e.to_string())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn get_clips(
    filter_id: Option<String>,
    limit: i64,
    offset: i64,
    after_id: Option<String>,
    preview_only: Option<bool>,
    content_filter: Option<String>,
    date_from: Option<String>,
    date_to: Option<String>,
    source_app: Option<String>,
    db: tauri::State<'_, Arc<Database>>,
) -> Result<Vec<ClipboardItem>, String> {
    get_clips_paged(
        filter_id,
        limit,
        offset,
        preview_only,
        content_filter,
        date_from,
        date_to,
        source_app,
        after_id,
        db.inner(),
    )
    .await
}

/// Release builds persist Info logs under LogDir. The History source-app
/// filter is a record of which applications the user copied from, and the
/// value itself is user-controlled, so Info must not print it. SBS-773.
///
/// Three states: not asked is not blank, and neither is a real selection.
fn source_app_filter_log_state(source_app: Option<&str>) -> &'static str {
    match source_app {
        None => "none",
        Some(app) if app.trim().is_empty() => "blank",
        Some(_) => "set",
    }
}

fn get_clips_request_log(
    filter_id: Option<&str>,
    preview_only: bool,
    content_filter: Option<&str>,
    date_from: Option<&str>,
    date_to: Option<&str>,
    source_app: Option<&str>,
) -> String {
    format!(
        "get_clips called with filter_id: {:?}, preview_only: {}, content_filter: {:?}, date: {:?}..{:?}, source_app: {}",
        filter_id,
        preview_only,
        content_filter,
        date_from,
        date_to,
        source_app_filter_log_state(source_app)
    )
}

// Cursor paging moved production onto the *_paged entry points, leaving
// this as the no-cursor adapter the existing offset-path tests call.
// cfg(test) rather than deleted: it is genuinely a test helper now, and
// inlining `None` into ~35 call sites would be churn, not clarity.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
async fn get_clips_in_database(
    filter_id: Option<String>,
    limit: i64,
    offset: i64,
    preview_only: Option<bool>,
    content_filter: Option<String>,
    date_from: Option<String>,
    date_to: Option<String>,
    source_app: Option<String>,
    db: &Database,
) -> Result<Vec<ClipboardItem>, String> {
    get_clips_paged(
        filter_id,
        limit,
        offset,
        preview_only,
        content_filter,
        date_from,
        date_to,
        source_app,
        None,
        db,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn get_clips_paged(
    filter_id: Option<String>,
    limit: i64,
    offset: i64,
    preview_only: Option<bool>,
    content_filter: Option<String>,
    date_from: Option<String>,
    date_to: Option<String>,
    source_app: Option<String>,
    after_id: Option<String>,
    db: &Database,
) -> Result<Vec<ClipboardItem>, String> {
    let pool = &db.pool;
    let preview_only = preview_only.unwrap_or(false);
    let started = Instant::now();

    log::info!(
        "{}",
        get_clips_request_log(
            filter_id.as_deref(),
            preview_only,
            content_filter.as_deref(),
            date_from.as_deref(),
            date_to.as_deref(),
            source_app.as_deref(),
        )
    );
    // Debug-only structured fields: state and length, never the raw value.
    // Release LogDir is Info, so this does not persist. SBS-773.
    log::debug!(
        "get_clips source_app filter state={} byte_len={}",
        source_app_filter_log_state(source_app.as_deref()),
        source_app.as_deref().map(str::len).unwrap_or(0)
    );

    let folder_id = match filter_id.as_deref() {
        Some(id) => match id.parse::<i64>() {
            Ok(id) => Some(id),
            Err(_) => {
                log::info!("Unknown folder_id, returning empty");
                return Ok(Vec::new());
            }
        },
        None => None,
    };
    let source_app = source_app.filter(|app| !app.trim().is_empty());
    let where_body = clip_where_body(
        folder_id,
        content_filter.as_deref(),
        date_from.as_deref(),
        date_to.as_deref(),
    );
    // `offset` is a source-row index. Clients that count displayed rows also
    // send `after_id` (the last row they have) so a skipped unreadable neighbor
    // cannot shift later pages. Limit is still a readable-row count: a
    // decrypt failure fills forward instead of returning a short page.
    let requested_offset = offset.max(0) as usize;
    let requested_limit = limit.max(0) as usize;
    let after_id = listing_cursor_id(after_id.as_deref()).map(str::to_string);

    let sql_started = Instant::now();
    let clips: Vec<Clip> = if let Some(app) = source_app.as_deref() {
        // The app name is encrypted with a random nonce, so SQL can neither
        // match nor group on it. Take the ordered ids from the database, narrow
        // them against the in-memory index, and page the result — the same
        // shape search uses, so ordering and pinning stay authoritative in SQL
        // and a filtered page can't hide matches on unfetched pages.
        db.search_index.ensure_ready(pool, &db.crypto).await?;
        let allowed = db.search_index.ids_for_source_app(app);
        if allowed.is_empty() {
            Vec::new()
        } else {
            let ordered_ids = fetch_ordered_clip_ids(
                pool,
                &where_body,
                folder_id,
                date_from.as_deref(),
                date_to.as_deref(),
            )
            .await?;
            let matching: Vec<String> = ordered_ids
                .into_iter()
                .filter(|id| allowed.contains(id))
                .collect();
            let source_start =
                source_start_for_ids(&matching, after_id.as_deref(), requested_offset);
            collect_readable_clips_by_id(db, &matching, source_start, requested_limit).await?
        }
    } else {
        let source_start = if after_id.is_some() {
            let ordered_ids = fetch_ordered_clip_ids(
                pool,
                &where_body,
                folder_id,
                date_from.as_deref(),
                date_to.as_deref(),
            )
            .await?;
            source_start_for_ids(&ordered_ids, after_id.as_deref(), requested_offset)
        } else {
            requested_offset
        };
        let list_sql = format!(
            "SELECT * FROM clips WHERE {where_body} \
             ORDER BY is_pinned DESC, created_at DESC, uuid DESC LIMIT ? OFFSET ?"
        );
        let date_from = date_from.as_deref();
        let date_to = date_to.as_deref();
        collect_readable_clips(db, source_start, requested_limit, |start, count| {
            let sql = list_sql.as_str();
            async move {
                let mut query = sqlx::query_as::<_, Clip>(sql);
                if let Some(id) = folder_id {
                    query = query.bind(id);
                }
                if let Some(from) = date_from {
                    query = query.bind(from.to_string());
                }
                if let Some(to) = date_to {
                    query = query.bind(to.to_string());
                }
                let rows = query
                    .bind(count as i64)
                    .bind(start as i64)
                    .fetch_all(pool)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(ClipBatch {
                    exhausted: rows.len() < count,
                    rows,
                })
            }
        })
        .await?
    };
    let sql_ms = sql_started.elapsed().as_millis();

    log::info!("DB: Found {} readable clips", clips.len());

    let image_rows = clips
        .iter()
        .filter(|clip| clip.clip_type == "image")
        .count();
    let raw_bytes: usize = clips.iter().map(|clip| clip.content.len()).sum();
    let map_started = Instant::now();
    let items: Vec<ClipboardItem> = clips
        .iter()
        .enumerate()
        .map(|(idx, clip)| {
            let item = clip_to_list_item(clip, preview_only);
            // Only log first 10 clips to reduce noise
            if idx < 10 {
                log::trace!(
                    "{} Clip {}: type='{}', content_len={}",
                    idx,
                    clip.uuid,
                    clip.clip_type,
                    item.content.len()
                );
            }
            item
        })
        .collect();
    let map_ms = map_started.elapsed().as_millis();
    let total_ms = started.elapsed().as_millis();
    log::info!(
        "[perf][get_clips] sql_ms={} map_ms={} total_ms={} rows={} images={} raw_bytes={} preview_only={} filter_id={:?} offset={} limit={}",
        sql_ms,
        map_ms,
        total_ms,
        clips.len(),
        image_rows,
        raw_bytes,
        preview_only,
        filter_id,
        requested_offset,
        requested_limit
    );

    Ok(items)
}

fn restore_hash_material(
    clip: &Clip,
    full_image: Option<&[u8]>,
    formats: &[(String, Vec<u8>)],
    plain_text: bool,
) -> Vec<u8> {
    if plain_text {
        let mut material = Vec::new();
        material.extend_from_slice(b"text");
        material.push(0);
        material.extend_from_slice(&clip.content);
        return material;
    }

    crate::clipboard::build_clip_hash_material(
        &clip.clip_type,
        full_image.unwrap_or(&clip.content),
        formats
            .iter()
            .map(|(format, content)| (format.as_str(), content.as_slice())),
    )
}

/// Record that the user actually used this clip, bumping it to the top of
/// history and restarting its retention clock.
///
/// `created_at` doubles as "last used" for both ordering and retention, so this
/// only applies when the clipboard write landed. An unconditional bump reordered
/// history and extended the life of a clip after a paste that never happened.
async fn mark_clip_used(pool: &sqlx::SqlitePool, uuid: &str, write_succeeded: bool) {
    if !write_succeeded {
        return;
    }
    let _ = sqlx::query(r#"UPDATE clips SET created_at = CURRENT_TIMESTAMP WHERE uuid = ?"#)
        .bind(uuid)
        .execute(pool)
        .await;
}

async fn restore_clip(
    id: &str,
    plain_text: bool,
    should_paste: bool,
    window: &tauri::WebviewWindow,
    db: &Database,
) -> Result<(), String> {
    let restore_started = Instant::now();
    let pool = &db.pool;

    let clip: Option<Clip> = sqlx::query_as(r#"SELECT * FROM clips WHERE uuid = ?"#)
        .bind(id)
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())?;

    match clip {
        Some(mut clip) => {
            decrypt_clip_fields(db, &mut clip)?;
            if plain_text && clip.clip_type == "image" {
                return Err("Plain text is not available for image clips".to_string());
            }

            // Synchronize clipboard access across the app
            let _guard = crate::clipboard::CLIPBOARD_SYNC.lock().await;

            let formats = if clip.clip_type == "image" || plain_text {
                Vec::new()
            } else {
                // Normalize HTML to the document form a re-capture of our own
                // clipboard write reads back (bare legacy fragments gain the
                // standard container), so the ignore hash below matches it.
                load_clip_formats(db, &clip.uuid)
                    .await?
                    .into_iter()
                    .map(|(format, content)| {
                        if format == "html" {
                            if let Ok(html) = std::str::from_utf8(&content) {
                                let document = crate::cf_html::document(html);
                                return (format, document.into_bytes());
                            }
                        }
                        (format, content)
                    })
                    .collect::<Vec<_>>()
            };
            let full_image = if clip.clip_type == "image" {
                Some(load_full_image_content(db, &mut clip).await?)
            } else {
                None
            };
            let hash_material =
                restore_hash_material(&clip, full_image.as_deref(), &formats, plain_text);
            let content_hash = crate::clipboard::calculate_hash(&hash_material);
            let uuid = clip.uuid.clone();

            crate::clipboard::set_ignore_hash(content_hash.clone());
            let clipboard_write_started = Instant::now();
            let final_res = if let Some(image) = full_image.as_deref() {
                crate::clipboard::set_clipboard_image_png(image)
                    .map_err(|error| format!("Failed to restore clipboard image: {error}"))
            } else {
                let clipboard_contents =
                    clipboard_contents_for_restore(&clip, None, &formats, plain_text)?;
                ClipboardContext::new()
                    .and_then(|context| context.set(clipboard_contents))
                    .map_err(|error| format!("Failed to restore clipboard formats: {error}"))
            };
            log::info!(
                "[perf][restore_clip] type={} image_bytes={} clipboard_write_ms={} total_ms={} success={}",
                clip.clip_type,
                full_image.as_ref().map_or(0, Vec::len),
                clipboard_write_started.elapsed().as_millis(),
                restore_started.elapsed().as_millis(),
                final_res.is_ok(),
            );
            if final_res.is_err() {
                crate::clipboard::clear_ignore_hash_if_matches(&content_hash);
            }

            mark_clip_used(pool, &uuid, final_res.is_ok()).await;

            if final_res.is_ok() {
                let remote_paste_mode = window
                    .state::<Arc<crate::settings_manager::SettingsManager>>()
                    .get()
                    .remote_paste_mode;
                let content = if clip.clip_type == "image" {
                    "[Image]".to_string()
                } else {
                    String::from_utf8_lossy(&clip.content).to_string()
                };
                let _ = window.emit("clipboard-write", &content);

                if should_paste {
                    crate::animate_window_hide(
                        window,
                        Some(Box::new(move || {
                            let strategy = crate::paste_engine::previous_paste_strategy();
                            if !crate::restore_previous_foreground_window() {
                                // Synthesizing Ctrl+V now would paste into
                                // whatever window happens to hold focus, which
                                // is not the one the user chose. The clip is
                                // already on the clipboard, so leave it for a
                                // manual paste instead of guessing a target.
                                log::warn!(
                                    "PASTE: focus was not restored; clipboard is ready for a manual Ctrl+V"
                                );
                                return;
                            }
                            if !crate::paste_engine::should_auto_paste_with_mode(
                                strategy,
                                &remote_paste_mode,
                            ) {
                                log::info!(
                                    "PASTE: Ninja clipboard is ready; waiting for physical Ctrl+V"
                                );
                                return;
                            }
                            std::thread::sleep(crate::paste_engine::paste_settle_delay(strategy));
                            crate::paste_engine::send_paste_input(strategy);
                        })),
                    );
                } else {
                    hide_flyout_after_copy(window);
                }
            }
            final_res
        }
        None => Err("Clip not found".to_string()),
    }
}

async fn load_recognized_text(db: &Database, id: &str) -> Result<String, String> {
    let encrypted: Option<String> = sqlx::query_scalar(
        "SELECT ocr_text FROM clips WHERE uuid = ? AND is_deleted = 0 AND clip_type = 'image'",
    )
    .bind(id)
    .fetch_optional(&db.pool)
    .await
    .map_err(|error| error.to_string())?
    .flatten();

    let text = encrypted
        .map(|value| db.crypto.decrypt_text(&value))
        .transpose()?
        .ok_or_else(|| "Recognized text is not available for this image".to_string())?;
    if text.trim().is_empty() {
        return Err("Recognized text is not available for this image".to_string());
    }
    Ok(text)
}

async fn restore_recognized_text(
    id: &str,
    should_paste: bool,
    window: &tauri::WebviewWindow,
    db: &Database,
) -> Result<(), String> {
    let text = load_recognized_text(db, id).await?;
    let _guard = crate::clipboard::CLIPBOARD_SYNC.lock().await;
    let mut hash_material = b"text\0".to_vec();
    hash_material.extend_from_slice(text.as_bytes());
    let content_hash = crate::clipboard::calculate_hash(&hash_material);
    crate::clipboard::set_ignore_hash(content_hash.clone());
    if let Err(error) = ClipboardContext::new()
        .and_then(|context| context.set(vec![ClipboardContent::Text(text.clone())]))
    {
        crate::clipboard::clear_ignore_hash_if_matches(&content_hash);
        return Err(format!("Failed to copy recognized text: {error}"));
    }

    let _ = sqlx::query("UPDATE clips SET created_at = CURRENT_TIMESTAMP WHERE uuid = ?")
        .bind(id)
        .execute(&db.pool)
        .await;
    let _ = window.emit("clipboard-write", &text);

    if should_paste {
        let remote_paste_mode = window
            .state::<Arc<crate::settings_manager::SettingsManager>>()
            .get()
            .remote_paste_mode;
        crate::animate_window_hide(
            window,
            Some(Box::new(move || {
                let strategy = crate::paste_engine::previous_paste_strategy();
                if !crate::restore_previous_foreground_window() {
                    // See the note in restore_clip: pasting into an unknown
                    // foreground window is worse than not pasting at all.
                    log::warn!(
                        "PASTE: focus was not restored; recognized text is ready for a manual Ctrl+V"
                    );
                    return;
                }
                if !crate::paste_engine::should_auto_paste_with_mode(strategy, &remote_paste_mode) {
                    log::info!("PASTE: Recognized text is ready; waiting for physical Ctrl+V");
                    return;
                }
                std::thread::sleep(crate::paste_engine::paste_settle_delay(strategy));
                crate::paste_engine::send_paste_input(strategy);
            })),
        );
    } else {
        hide_flyout_after_copy(window);
    }
    Ok(())
}

/// Dismiss the flyout once a copy has landed on the clipboard — its whole job
/// was to hand something over, so staying open would be in the way. Only the
/// flyout: the History window is a place you work in, and hiding it (or taking
/// the shared show/hide animation lock on its behalf) after every copy would be
/// wrong.
fn hide_flyout_after_copy(window: &tauri::WebviewWindow) {
    if window.label() == "main" {
        crate::animate_window_hide(window, None);
    }
}

#[tauri::command]
pub async fn paste_clip(
    id: String,
    plain_text: bool,
    window: tauri::WebviewWindow,
    db: tauri::State<'_, Arc<Database>>,
) -> Result<(), String> {
    restore_clip(&id, plain_text, true, &window, db.inner()).await
}

#[tauri::command]
pub async fn copy_clip(
    id: String,
    plain_text: bool,
    window: tauri::WebviewWindow,
    db: tauri::State<'_, Arc<Database>>,
) -> Result<(), String> {
    restore_clip(&id, plain_text, false, &window, db.inner()).await
}

#[tauri::command]
pub async fn paste_ocr_text(
    id: String,
    window: tauri::WebviewWindow,
    db: tauri::State<'_, Arc<Database>>,
) -> Result<(), String> {
    restore_recognized_text(&id, true, &window, db.inner()).await
}

#[tauri::command]
pub async fn copy_ocr_text(
    id: String,
    window: tauri::WebviewWindow,
    db: tauri::State<'_, Arc<Database>>,
) -> Result<(), String> {
    restore_recognized_text(&id, false, &window, db.inner()).await
}

#[tauri::command]
pub async fn delete_clip(
    id: String,
    hard_delete: bool,
    db: tauri::State<'_, Arc<Database>>,
) -> Result<(), String> {
    delete_clip_in_database(db.inner(), &id, hard_delete).await
}

/// Delete one clip, factored out so bulk delete runs exactly this path instead
/// of a faster bespoke one that could drift on image-file cleanup, capture
/// dedup, or search-index maintenance (SOU-583).
async fn delete_clip_in_database(db: &Database, id: &str, hard_delete: bool) -> Result<(), String> {
    let pool = &db.pool;

    if hard_delete {
        let mut transaction = pool.begin().await.map_err(|e| e.to_string())?;
        let file_path: Option<String> =
            sqlx::query_scalar(r#"SELECT file_path FROM clip_images WHERE clip_uuid = ?"#)
                .bind(id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(|e| e.to_string())?;
        sqlx::query(r#"DELETE FROM clips WHERE uuid = ?"#)
            .bind(id)
            .execute(&mut *transaction)
            .await
            .map_err(|e| e.to_string())?;
        transaction.commit().await.map_err(|e| e.to_string())?;

        remove_clip_image_files(&db.image_dir, file_path.into_iter().collect());
    } else {
        sqlx::query(r#"UPDATE clips SET is_deleted = 1 WHERE uuid = ?"#)
            .bind(id)
            .execute(pool)
            .await
            .map_err(|e| e.to_string())?;
    }
    crate::clipboard::reset_capture_dedup();
    db.search_index.remove(id);
    Ok(())
}

/// Delete several clips through the single-clip path, one at a time.
///
/// Returns how many were removed. A clip that fails is logged and skipped
/// rather than aborting the batch — a bulk cleanup that stops halfway with no
/// indication of where would be worse than finishing and reporting the count.
/// Only a batch where nothing at all succeeded surfaces as an error.
#[tauri::command]
pub async fn delete_clips(
    ids: Vec<String>,
    hard_delete: bool,
    db: tauri::State<'_, Arc<Database>>,
) -> Result<usize, String> {
    let mut deleted = 0usize;
    let mut first_error: Option<String> = None;
    for id in &ids {
        match delete_clip_in_database(db.inner(), id, hard_delete).await {
            Ok(()) => deleted += 1,
            Err(error) => {
                log::warn!("BULK: Failed to delete clip {id}: {error}");
                first_error.get_or_insert(error);
            }
        }
    }
    match first_error {
        Some(error) if deleted == 0 => Err(error),
        _ => Ok(deleted),
    }
}

/// Pin or unpin several clips. Unlike delete this is a plain column write with
/// no side effects, so one statement is both correct and the same semantics as
/// the single-clip toggle — it just sets an explicit state rather than flipping
/// each, so a mixed selection ends up uniform instead of inverted.
#[tauri::command]
pub async fn set_clips_pinned(
    ids: Vec<String>,
    pinned: bool,
    db: tauri::State<'_, Arc<Database>>,
) -> Result<u64, String> {
    if ids.is_empty() {
        return Ok(0);
    }
    let placeholders = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("UPDATE clips SET is_pinned = ? WHERE uuid IN ({placeholders})");
    let mut query = sqlx::query(&sql).bind(pinned);
    for id in &ids {
        query = query.bind(id);
    }
    let result = query
        .execute(&db.pool)
        .await
        .map_err(|error| error.to_string())?;
    Ok(result.rows_affected())
}

/// Move several clips into a folder, or out of one when `folder_id` is None.
#[tauri::command]
pub async fn move_clips_to_folder(
    ids: Vec<String>,
    folder_id: Option<String>,
    db: tauri::State<'_, Arc<Database>>,
) -> Result<u64, String> {
    if ids.is_empty() {
        return Ok(0);
    }
    let folder_id = match folder_id {
        Some(id) => Some(id.parse::<i64>().map_err(|_| "Invalid folder ID")?),
        None => None,
    };
    let placeholders = std::iter::repeat_n("?", ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("UPDATE clips SET folder_id = ? WHERE uuid IN ({placeholders})");
    let mut query = sqlx::query(&sql).bind(folder_id);
    for id in &ids {
        query = query.bind(id);
    }
    let result = query
        .execute(&db.pool)
        .await
        .map_err(|error| error.to_string())?;
    Ok(result.rows_affected())
}

async fn toggle_clip_pin_in_pool(pool: &SqlitePool, id: &str) -> Result<bool, String> {
    let pinned: Option<i64> = sqlx::query_scalar(
        r#"
        UPDATE clips
        SET is_pinned = CASE is_pinned WHEN 0 THEN 1 ELSE 0 END
        WHERE uuid = ? AND is_deleted = 0
        RETURNING is_pinned
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|error| error.to_string())?;

    pinned
        .map(|value| value != 0)
        .ok_or_else(|| "Clipboard item not found".to_string())
}

#[tauri::command]
pub async fn toggle_clip_pin(
    id: String,
    db: tauri::State<'_, Arc<Database>>,
) -> Result<bool, String> {
    toggle_clip_pin_in_pool(&db.pool, &id).await
}

/// Save a corrected version of an image's recognized text (SOU-590).
///
/// The issue left the call open on whether a correction should be transient or
/// written back. Written back: if the engine misread a character, the same
/// misreading is what search has indexed, so a transient fix would leave the
/// clip permanently unfindable by its real text and give the user no way to
/// mend that. A correction is worth more to search than to one paste.
///
/// The per-word boxes are rewritten to the same tokens (SBS-1010). Leaving them
/// as the engine stored them meant drag-select and search highlights still
/// copied the misreading after "Recognized text updated". Geometry stays;
/// leftover boxes are dropped so a shorter correction cannot select a stale
/// word. Unreadable existing boxes are left unchanged rather than cleared —
/// failed to decrypt is not "no layout".
#[tauri::command]
pub async fn set_clip_ocr_text(
    id: String,
    text: String,
    db: tauri::State<'_, Arc<Database>>,
) -> Result<(), String> {
    set_clip_ocr_text_in_database(db.inner(), &id, &text).await
}

/// Whether a correction can rewrite the stored word-box JSON.
enum OcrWordsUpdate {
    /// Encrypt and store this JSON, or NULL the column.
    Replace(Option<String>),
    /// Existing ciphertext could not be read; leave the column alone.
    LeaveUnchanged,
}

fn rewrite_stored_ocr_words(
    crypto: &crate::crypto::CryptoManager,
    encrypted_words: Option<&str>,
    trimmed: &str,
) -> Result<OcrWordsUpdate, String> {
    if trimmed.is_empty() {
        // Clearing the assembled block must also clear selectable boxes, or
        // drag-select still copies the reading the user just deleted.
        return Ok(OcrWordsUpdate::Replace(None));
    }
    let Some(ciphertext) = encrypted_words else {
        return Ok(OcrWordsUpdate::Replace(None));
    };
    let json = match crypto.decrypt_text(ciphertext) {
        Ok(json) => json,
        Err(error) => {
            log::warn!(
                "CLIPS: Leaving unreadable OCR word boxes unchanged after a correction: {error}"
            );
            return Ok(OcrWordsUpdate::LeaveUnchanged);
        }
    };
    let layout: crate::ocr::OcrLayout = match serde_json::from_str(&json) {
        Ok(layout) => layout,
        Err(error) => {
            log::warn!(
                "CLIPS: Leaving unparseable OCR word boxes unchanged after a correction: {error}"
            );
            return Ok(OcrWordsUpdate::LeaveUnchanged);
        }
    };
    let layout = crate::ocr::apply_ocr_text_to_layout(layout, trimmed);
    if layout.words.is_empty() {
        return Ok(OcrWordsUpdate::Replace(None));
    }
    let json = serde_json::to_string(&layout).map_err(|error| error.to_string())?;
    Ok(OcrWordsUpdate::Replace(Some(crypto.encrypt_text(&json)?)))
}

async fn set_clip_ocr_text_in_database(db: &Database, id: &str, text: &str) -> Result<(), String> {
    let row: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT clip_type, ocr_words FROM clips WHERE uuid = ?")
            .bind(id)
            .fetch_optional(&db.pool)
            .await
            .map_err(|e| e.to_string())?;
    let Some((clip_type, encrypted_words)) = row else {
        return Err("Recognized text belongs to image clips".to_string());
    };
    if clip_type != "image" {
        return Err("Recognized text belongs to image clips".to_string());
    }

    let trimmed = text.trim();
    let stored = if trimmed.is_empty() {
        None
    } else {
        db.crypto.encrypt_optional_text(Some(trimmed))?
    };
    let words_update = rewrite_stored_ocr_words(&db.crypto, encrypted_words.as_deref(), trimmed)?;
    // A corrected clip counts as processed: leaving it pending would let the
    // background worker overwrite the correction on its next pass.
    match words_update {
        OcrWordsUpdate::Replace(stored_words) => {
            sqlx::query(
                "UPDATE clips SET ocr_text = ?, ocr_words = ?, ocr_status = 'completed' WHERE uuid = ?",
            )
            .bind(&stored)
            .bind(&stored_words)
            .bind(id)
            .execute(&db.pool)
            .await
            .map_err(|e| e.to_string())?;
        }
        OcrWordsUpdate::LeaveUnchanged => {
            sqlx::query("UPDATE clips SET ocr_text = ?, ocr_status = 'completed' WHERE uuid = ?")
                .bind(&stored)
                .bind(id)
                .execute(&db.pool)
                .await
                .map_err(|e| e.to_string())?;
        }
    }

    db.search_index.update_ocr(id, trimmed);
    Ok(())
}

/// Replace a text clip's content in place (SOU-587), for the copy-fix-paste
/// workflow that otherwise means pasting first and editing at the destination.
#[tauri::command]
pub async fn update_clip_text(
    id: String,
    text: String,
    db: tauri::State<'_, Arc<Database>>,
) -> Result<(), String> {
    update_clip_text_in_database(db.inner(), &id, &text).await
}

async fn update_clip_text_in_database(db: &Database, id: &str, text: &str) -> Result<(), String> {
    let clip_type: Option<String> =
        sqlx::query_scalar("SELECT clip_type FROM clips WHERE uuid = ?")
            .bind(id)
            .fetch_optional(&db.pool)
            .await
            .map_err(|e| e.to_string())?;
    let clip_type = clip_type.ok_or_else(|| format!("Clip {id} not found"))?;
    if clip_type == "image" {
        return Err("Image clips cannot be edited".to_string());
    }

    let hash_material = crate::clipboard::build_clip_hash_material(
        "text",
        text.as_bytes(),
        std::iter::empty::<(&str, &[u8])>(),
    );
    // Dedup treats an edited clip exactly like any other clip: the hash follows
    // the content, so re-copying this text later matches this row instead of
    // adding a third. A collision with a *different* clip is rejected with no
    // mutation: deleting HTML/RTF first and then failing the unique hash left
    // a half-edited row (SBS-768).
    let content_hash = db.crypto.keyed_hash(&hash_material);
    let preview = truncate_preview(text);
    let encrypted_content = db.crypto.encrypt(text.as_bytes())?;
    let encrypted_preview = db.crypto.encrypt_text(&preview)?;

    let mut transaction = db.pool.begin().await.map_err(|e| e.to_string())?;

    let existing_uuid: Option<String> =
        sqlx::query_scalar("SELECT uuid FROM clips WHERE content_hash = ? AND uuid != ?")
            .bind(&content_hash)
            .bind(id)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(|e| e.to_string())?;
    if existing_uuid.is_some() {
        return Err("Another clip already has this text".to_string());
    }

    // The edited text is now the whole clip. Any html/rtf captured alongside it
    // described the *old* text, so keeping them would paste the pre-edit version
    // into anything that prefers a rich format — the edit would look like it had
    // silently failed. Dropping them makes the clip plain text, which is what
    // was actually edited. This is also why the hash is computed over no extra
    // formats: it has to describe what the clip now is.
    sqlx::query("DELETE FROM clip_formats WHERE clip_uuid = ?")
        .bind(id)
        .execute(&mut *transaction)
        .await
        .map_err(|e| e.to_string())?;

    let update = sqlx::query(
        "UPDATE clips SET clip_type = 'text', content = ?, text_preview = ?, content_hash = ? WHERE uuid = ?",
    )
    .bind(encrypted_content)
    .bind(encrypted_preview)
    .bind(&content_hash)
    .bind(id)
    .execute(&mut *transaction)
    .await;

    match update {
        Ok(_) => {}
        Err(error) if is_unique_constraint_error(&error) => {
            return Err("Another clip already has this text".to_string());
        }
        Err(error) => return Err(error.to_string()),
    }

    transaction.commit().await.map_err(|e| e.to_string())?;

    // The index holds the pre-edit text until it is told otherwise, so search
    // would keep matching words the clip no longer contains.
    db.search_index
        .upsert(id, "text", text.as_bytes(), &preview, None, None);
    // A capture of the old text would otherwise be suppressed as a duplicate of
    // what this row used to hold.
    crate::clipboard::reset_capture_dedup();
    Ok(())
}

fn is_unique_constraint_error(error: &sqlx::Error) -> bool {
    match error {
        sqlx::Error::Database(database_error) => {
            sqlx::error::DatabaseError::is_unique_violation(database_error.as_ref())
                || sqlx::error::DatabaseError::message(database_error.as_ref())
                    .to_ascii_lowercase()
                    .contains("unique")
        }
        _ => error.to_string().to_ascii_lowercase().contains("unique"),
    }
}

/// Preview text stored on the row, bounded so a huge clip does not bloat every
/// list query. Same helper and limit as capture and Ditto import (SBS-994).
fn truncate_preview(text: &str) -> String {
    crate::clip_list::truncate_text_preview(text)
}

/// Attach or clear a clip's note (SOU-588). An empty note clears the field
/// rather than storing a blank string, so "has a note" stays a real distinction.
#[tauri::command]
pub async fn set_clip_notes(
    id: String,
    notes: String,
    db: tauri::State<'_, Arc<Database>>,
) -> Result<(), String> {
    set_clip_notes_in_database(db.inner(), &id, &notes).await
}

async fn set_clip_notes_in_database(db: &Database, id: &str, notes: &str) -> Result<(), String> {
    let trimmed = notes.trim();
    // The input carries maxLength, so this is the guard for anything that is not
    // that input. Counted in chars, not bytes, so the limit means the same thing
    // for a note that is not ASCII.
    if trimmed.chars().count() > NOTE_CHAR_LIMIT {
        return Err(format!("A note is limited to {NOTE_CHAR_LIMIT} characters"));
    }
    let stored = if trimmed.is_empty() {
        None
    } else {
        db.crypto.encrypt_optional_text(Some(trimmed))?
    };

    let affected = sqlx::query("UPDATE clips SET notes = ? WHERE uuid = ?")
        .bind(&stored)
        .bind(id)
        .execute(&db.pool)
        .await
        .map_err(|e| e.to_string())?
        .rows_affected();
    if affected == 0 {
        return Err(format!("Clip {id} not found"));
    }

    // Notes are searchable, so the index has to learn about them the moment they
    // change — otherwise a note would only become findable after a rebuild.
    db.search_index.update_notes(id, trimmed);
    Ok(())
}

/// Hide or unhide a clip's content in the list (SOU-586). Returns the new state.
///
/// Deliberately the lighter-weight sibling of a master-password lock: no new
/// auth or key material, just a display flag. The content keeps its existing
/// AES-256-GCM encryption at rest, stays searchable, and still pastes in full.
#[tauri::command]
pub async fn toggle_clip_hidden(
    id: String,
    db: tauri::State<'_, Arc<Database>>,
) -> Result<bool, String> {
    toggle_clip_hidden_in_pool(&db.pool, &id).await
}

async fn toggle_clip_hidden_in_pool(pool: &SqlitePool, id: &str) -> Result<bool, String> {
    // Flip and read back in one statement. Reading then writing would let two
    // concurrent toggles both observe the old value and write the same new one,
    // losing a toggle and returning the wrong state to one of the callers.
    let next: Option<bool> = sqlx::query_scalar(
        "UPDATE clips SET is_hidden = NOT is_hidden WHERE uuid = ? RETURNING is_hidden",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(|e| e.to_string())?;
    next.ok_or_else(|| format!("Clip {id} not found"))
}

#[tauri::command]
pub async fn move_to_folder(
    clip_id: String,
    folder_id: Option<String>,
    db: tauri::State<'_, Arc<Database>>,
) -> Result<(), String> {
    let pool = &db.pool;

    let folder_id = match folder_id {
        Some(id) => Some(id.parse::<i64>().map_err(|_| "Invalid folder ID")?),
        None => None,
    };

    sqlx::query(r#"UPDATE clips SET folder_id = ? WHERE uuid = ?"#)
        .bind(folder_id)
        .bind(&clip_id)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn create_folder(
    name: String,
    icon: Option<String>,
    color: Option<String>,
    db: tauri::State<'_, Arc<Database>>,
    window: tauri::WebviewWindow,
) -> Result<FolderItem, String> {
    let pool = &db.pool;

    // Check if folder with same name exists (excluding system folders if we wanted, but name uniqueness is good generally)
    let exists: Option<i64> = sqlx::query_scalar("SELECT 1 FROM folders WHERE name = ?")
        .bind(&name)
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())?;

    if exists.is_some() {
        return Err("A folder with this name already exists".to_string());
    }

    let id = sqlx::query(r#"INSERT INTO folders (name, icon, color) VALUES (?, ?, ?)"#)
        .bind(&name)
        .bind(icon.as_ref())
        .bind(color.as_ref())
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?
        .last_insert_rowid();

    let _ = window.emit("clipboard-change", ());

    Ok(FolderItem {
        id: id.to_string(),
        name,
        icon,
        color,
        is_system: false,
        item_count: 0,
    })
}

#[tauri::command]
pub async fn delete_folder(
    id: String,
    db: tauri::State<'_, Arc<Database>>,
    window: tauri::WebviewWindow,
) -> Result<(), String> {
    let folder_id: i64 = id.parse().map_err(|_| "Invalid folder ID")?;
    delete_folder_in_pool(&db.pool, folder_id).await?;
    let _ = window.emit("clipboard-change", ());
    Ok(())
}

/// Unfile every member, then drop the folder, in one transaction.
///
/// `clips.folder_id` references `folders(id)` with no ON DELETE action, and the
/// pool has foreign keys on. Deleting the folder row first fails for any folder
/// that still has clips.
async fn delete_folder_in_pool(pool: &SqlitePool, folder_id: i64) -> Result<(), String> {
    let mut transaction = pool.begin().await.map_err(|e| e.to_string())?;
    sqlx::query(r#"UPDATE clips SET folder_id = NULL WHERE folder_id = ?"#)
        .bind(folder_id)
        .execute(&mut *transaction)
        .await
        .map_err(|e| e.to_string())?;
    sqlx::query(r#"DELETE FROM folders WHERE id = ?"#)
        .bind(folder_id)
        .execute(&mut *transaction)
        .await
        .map_err(|e| e.to_string())?;
    transaction.commit().await.map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn rename_folder(
    id: String,
    name: String,
    db: tauri::State<'_, Arc<Database>>,
    window: tauri::WebviewWindow,
) -> Result<(), String> {
    let pool = &db.pool;

    let folder_id: i64 = id.parse().map_err(|_| "Invalid folder ID")?;

    // Check availability
    let exists: Option<i64> =
        sqlx::query_scalar("SELECT 1 FROM folders WHERE name = ? AND id != ?")
            .bind(&name)
            .bind(folder_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| e.to_string())?;

    if exists.is_some() {
        return Err("A folder with this name already exists".to_string());
    }

    sqlx::query(r#"UPDATE folders SET name = ? WHERE id = ?"#)
        .bind(name)
        .bind(folder_id)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;

    // Emit event so main window knows to refresh
    let _ = window.emit("clipboard-change", ());
    Ok(())
}

#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn search_clips(
    query: String,
    filter_id: Option<String>,
    limit: i64,
    offset: i64,
    after_id: Option<String>,
    preview_only: Option<bool>,
    content_filter: Option<String>,
    date_from: Option<String>,
    date_to: Option<String>,
    source_app: Option<String>,
    db: tauri::State<'_, Arc<Database>>,
) -> Result<Vec<ClipboardItem>, String> {
    search_clips_paged(
        query,
        filter_id,
        limit,
        offset,
        preview_only,
        content_filter,
        date_from,
        date_to,
        source_app,
        after_id,
        db.inner(),
    )
    .await
}

// Cursor paging moved production onto the *_paged entry points, leaving
// this as the no-cursor adapter the existing offset-path tests call.
// cfg(test) rather than deleted: it is genuinely a test helper now, and
// inlining `None` into ~35 call sites would be churn, not clarity.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
async fn search_clips_in_database(
    query: String,
    filter_id: Option<String>,
    limit: i64,
    offset: i64,
    preview_only: Option<bool>,
    content_filter: Option<String>,
    date_from: Option<String>,
    date_to: Option<String>,
    source_app: Option<String>,
    db: &Database,
) -> Result<Vec<ClipboardItem>, String> {
    search_clips_paged(
        query,
        filter_id,
        limit,
        offset,
        preview_only,
        content_filter,
        date_from,
        date_to,
        source_app,
        None,
        db,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn search_clips_paged(
    query: String,
    filter_id: Option<String>,
    limit: i64,
    offset: i64,
    preview_only: Option<bool>,
    content_filter: Option<String>,
    date_from: Option<String>,
    date_to: Option<String>,
    source_app: Option<String>,
    after_id: Option<String>,
    db: &Database,
) -> Result<Vec<ClipboardItem>, String> {
    // Flyout and History are the only search callers, and both are list UIs.
    // Unlike get_clips, omitting the flag withholds the body: a forgotten
    // previewOnly on a keystroke path must not resurrect the SBS-912 leak.
    let preview_only = crate::clip_list::resolve_search_preview_only(preview_only);
    let pool = &db.pool;
    let started = Instant::now();
    let requested_offset = offset.max(0) as usize;
    let requested_limit = limit.max(0) as usize;
    let after_id = listing_cursor_id(after_id.as_deref()).map(str::to_string);
    if requested_limit == 0 {
        return Ok(Vec::new());
    }
    let folder_id = match filter_id.as_deref() {
        Some(id) => match id.parse::<i64>() {
            Ok(id) => Some(id),
            Err(_) => return Ok(Vec::new()),
        },
        None => None,
    };
    let index_started = Instant::now();
    db.search_index.ensure_ready(pool, &db.crypto).await?;
    let mut candidates = db.search_index.matches(&query);
    // The source app is encrypted with a random nonce, so it narrows the
    // candidate set here alongside the text match rather than in SQL. AND, not
    // OR: every active filter has to hold.
    if let Some(app) = source_app.as_deref().filter(|app| !app.trim().is_empty()) {
        let allowed = db.search_index.ids_for_source_app(app);
        candidates.retain(|id| allowed.contains(id));
    }
    let index_ms = index_started.elapsed().as_millis();
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    // Ordering, pinning, folder filtering, date range, and pagination remain
    // authoritative in SQLite. Only UUIDs are scanned here; encrypted payloads
    // are fetched and decrypted for the final result page.
    let where_body = clip_where_body(
        folder_id,
        content_filter.as_deref(),
        date_from.as_deref(),
        date_to.as_deref(),
    );
    let sql_started = Instant::now();
    let ordered_ids = fetch_ordered_clip_ids(
        pool,
        &where_body,
        folder_id,
        date_from.as_deref(),
        date_to.as_deref(),
    )
    .await?;
    let matching = ordered_ids
        .into_iter()
        .filter(|id| candidates.contains(id))
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return Ok(Vec::new());
    }

    // An indexed hit can still fail to decrypt here (the index skips the
    // content of an image clip, so a corrupt thumbnail only surfaces now), so
    // the page is filled with readable rows rather than trimmed after the fact.
    let source_start = source_start_for_ids(&matching, after_id.as_deref(), requested_offset);
    let clips = collect_readable_clips_by_id(db, &matching, source_start, requested_limit).await?;
    let sql_ms = sql_started.elapsed().as_millis();

    let image_rows = clips
        .iter()
        .filter(|clip| clip.clip_type == "image")
        .count();
    let raw_bytes: usize = clips.iter().map(|clip| clip.content.len()).sum();
    let map_started = Instant::now();
    let items: Vec<ClipboardItem> = clips
        .iter()
        .map(|clip| clip_to_search_item(clip, &query, preview_only))
        .collect();
    let map_ms = map_started.elapsed().as_millis();
    let total_ms = started.elapsed().as_millis();
    log::info!(
        "[perf][search_clips] index_ms={} sql_ms={} map_ms={} total_ms={} candidates={} rows={} images={} raw_bytes={} preview_only={} filter_id={:?} offset={} limit={}",
        index_ms,
        sql_ms,
        map_ms,
        total_ms,
        candidates.len(),
        clips.len(),
        image_rows,
        raw_bytes,
        preview_only,
        filter_id,
        requested_offset,
        requested_limit
    );

    Ok(items)
}

#[tauri::command]
pub async fn get_folders(db: tauri::State<'_, Arc<Database>>) -> Result<Vec<FolderItem>, String> {
    let pool = &db.pool;

    let folders: Vec<Folder> = sqlx::query_as(r#"SELECT * FROM folders ORDER BY created_at"#)
        .fetch_all(pool)
        .await
        .map_err(|e| e.to_string())?;

    // Get counts for all folders in one query
    let counts: Vec<(i64, i64)> = sqlx::query_as(
        r#"
        SELECT folder_id, COUNT(*) as count
        FROM clips
        WHERE is_deleted = 0 AND folder_id IS NOT NULL
        GROUP BY folder_id
    "#,
    )
    .fetch_all(pool)
    .await
    .map_err(|e| e.to_string())?;

    // Create a map for easier lookup
    let count_map: HashMap<i64, i64> = counts.into_iter().collect();

    let items: Vec<FolderItem> = folders
        .iter()
        .map(|folder| FolderItem {
            id: folder.id.to_string(),
            name: folder.name.clone(),
            icon: folder.icon.clone(),
            color: folder.color.clone(),
            is_system: folder.is_system,
            item_count: *count_map.get(&folder.id).unwrap_or(&0),
        })
        .collect();

    //println!("folder items: {:#?}", items);

    Ok(items)
}

#[tauri::command]
pub async fn get_clipboard_history_size(
    db: tauri::State<'_, Arc<Database>>,
) -> Result<i64, String> {
    let pool = &db.pool;

    let count: i64 =
        sqlx::query_scalar::<_, i64>(r#"SELECT COUNT(*) FROM clips WHERE is_deleted = 0"#)
            .fetch_one(pool)
            .await
            .map_err(|e| e.to_string())?;
    Ok(count)
}

async fn clear_clips_in_pool(
    pool: &SqlitePool,
    preserve_pinned: bool,
) -> Result<(u64, Vec<String>), String> {
    let clip_filter = if preserve_pinned {
        "is_pinned = 0 OR is_deleted = 1"
    } else {
        "1 = 1"
    };
    let image_filter = if preserve_pinned {
        "clip_uuid NOT IN (SELECT uuid FROM clips WHERE is_pinned = 1 AND is_deleted = 0)"
    } else {
        "1 = 1"
    };
    let image_paths_sql = format!("SELECT file_path FROM clip_images WHERE {image_filter}");
    let delete_images_sql = format!("DELETE FROM clip_images WHERE {image_filter}");
    let delete_clips_sql = format!("DELETE FROM clips WHERE {clip_filter}");

    let mut transaction = pool.begin().await.map_err(|error| error.to_string())?;
    let image_paths: Vec<Option<String>> = sqlx::query_scalar(&image_paths_sql)
        .fetch_all(&mut *transaction)
        .await
        .map_err(|error| error.to_string())?;

    sqlx::query(&delete_images_sql)
        .execute(&mut *transaction)
        .await
        .map_err(|error| error.to_string())?;
    let deleted = sqlx::query(&delete_clips_sql)
        .execute(&mut *transaction)
        .await
        .map_err(|error| error.to_string())?
        .rows_affected();

    transaction
        .commit()
        .await
        .map_err(|error| error.to_string())?;

    Ok((
        deleted,
        image_paths
            .into_iter()
            .flatten()
            .filter(|path| !path.is_empty())
            .collect(),
    ))
}

#[derive(serde::Serialize)]
pub struct StorageUsage {
    pub items: i64,
    pub bytes: i64,
}

#[derive(serde::Serialize)]
pub struct StorageReclaim {
    pub freed_bytes: i64,
    pub usage: StorageUsage,
}

/// The Cubby history data directory: `cubby.db` (+ its `-wal`/`-shm` sidecars),
/// the `images/` blob directory, and `storage.key`. It is `image_dir`'s parent
/// (`image_dir` is `<data_dir>/images`). An installed run keeps its logs under
/// `%LOCALAPPDATA%`, well outside this directory, but a portable run writes them
/// to `<data_dir>/logs` (`lib.rs::portable_log_dir`), which is inside it. Callers
/// that measure this directory must exclude that log folder. See
/// `history_disk_bytes`.
fn history_data_dir(db: &Database) -> std::path::PathBuf {
    db.image_dir
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| db.image_dir.clone())
}

/// Recursively sum the size of every file under `path`. Missing or unreadable
/// entries are skipped rather than failing the whole measurement. Runs on a
/// blocking thread since it stat()s potentially thousands of image files.
///
/// `exclude` skips one exact directory path and everything under it. It is an
/// explicit path from the caller, not a name match, so a user folder that
/// happens to be called `logs` is still measured.
fn directory_size_bytes(path: &std::path::Path, exclude: Option<&std::path::Path>) -> i64 {
    let mut total: i64 = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            if metadata.is_dir() {
                let path = entry.path();
                if exclude.is_some_and(|skip| skip == path) {
                    continue;
                }
                stack.push(path);
            } else {
                total = total.saturating_add(metadata.len() as i64);
            }
        }
    }
    total
}

/// The log directory to leave out of a history measurement, if there is one.
///
/// Only a portable run puts logs inside the history data directory, and only
/// when that directory *is* the portable root. An installed run keeps them
/// under `%LOCALAPPDATA%`, so excluding `<history_dir>/logs` there would
/// silently drop a real user folder — or one left behind by a portable copy
/// someone moved — out of "Storage used" and the reclaim before/after.
///
/// The path comes from `portable_log_dir` itself, so the measurement and the
/// log location cannot drift apart.
fn excluded_log_dir(
    history_dir: &std::path::Path,
    portable_root: Option<std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
    let root = portable_root?;
    if root != history_dir {
        return None;
    }
    crate::portable_log_dir(Some(root))
}

/// Bytes of clipboard history on disk, portable logs excluded.
async fn history_disk_bytes(db: &Database) -> Result<i64, String> {
    let dir = history_data_dir(db);
    let logs = excluded_log_dir(&dir, crate::portable_data_dir());
    tokio::task::spawn_blocking(move || directory_size_bytes(&dir, logs.as_deref()))
        .await
        .map_err(|error| error.to_string())
}

/// Actual on-disk footprint of the clipboard history: the true size of the data
/// directory (database file including free pages and WAL, plus the image blob
/// files). This is what the user sees in Explorer, unlike a logical row sum,
/// which ignores SQLite free pages left behind after deletes.
#[tauri::command]
pub async fn get_storage_usage(
    db: tauri::State<'_, Arc<Database>>,
) -> Result<StorageUsage, String> {
    let items: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM clips WHERE is_deleted = 0"#)
        .fetch_one(&db.pool)
        .await
        .map_err(|error| error.to_string())?;
    let bytes = history_disk_bytes(&db).await?;
    Ok(StorageUsage { items, bytes })
}

/// Reclaim disk space: purge orphaned image blobs, then `VACUUM` the database to
/// return SQLite free pages to the OS and checkpoint the WAL so the `-wal` file
/// shrinks too. Without this, deleting history barely moves the on-disk size
/// because SQLite keeps freed pages in the file. Returns how much was freed
/// along with the refreshed usage.
#[tauri::command]
pub async fn reclaim_storage(
    db: tauri::State<'_, Arc<Database>>,
) -> Result<StorageReclaim, String> {
    let before = history_disk_bytes(&db).await?;

    // Drop clip_images rows (and their disk files) whose parent clip is gone, so
    // VACUUM isn't preserving blobs nothing references.
    cleanup_orphan_clip_image_files(&db.pool, &db.image_dir).await?;

    // VACUUM rewrites the database without free pages; it cannot run inside a
    // transaction. In WAL mode the rewrite lands in the -wal file, so checkpoint
    // with TRUNCATE afterwards to flush it back and shrink the sidecar on disk.
    sqlx::query("VACUUM")
        .execute(&db.pool)
        .await
        .map_err(|error| format!("Failed to compact the database: {error}"))?;
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(&db.pool)
        .await
        .map_err(|error| error.to_string())?;

    let items: i64 = sqlx::query_scalar(r#"SELECT COUNT(*) FROM clips WHERE is_deleted = 0"#)
        .fetch_one(&db.pool)
        .await
        .map_err(|error| error.to_string())?;
    let after = history_disk_bytes(&db).await?;

    Ok(StorageReclaim {
        freed_bytes: before.saturating_sub(after),
        usage: StorageUsage {
            items,
            bytes: after,
        },
    })
}

/// Apply the current retention settings immediately (rather than waiting for the
/// next capture), so lowering the window prunes right away and the storage
/// readout updates. Broadcasts `clipboard-change` so the flyout refreshes.
#[tauri::command]
pub async fn apply_retention(
    app: AppHandle,
    db: tauri::State<'_, Arc<Database>>,
    settings_manager: tauri::State<'_, Arc<crate::settings_manager::SettingsManager>>,
) -> Result<u64, String> {
    let settings = settings_manager.get();
    let (deleted, image_paths) =
        enforce_retention_in_pool(&db.pool, settings.max_items, settings.auto_delete_days).await?;
    // A preserve-only pass (SOU-244) drops image blobs without removing any
    // clips, so `deleted` can be 0 while blobs were still freed and rows flagged
    // expired. Refresh the flyout whenever anything changed so the new "text
    // only" badges appear; only a real removal needs the search index rebuilt.
    let blobs_removed = !image_paths.is_empty();
    remove_clip_image_files(&db.image_dir, image_paths);
    if deleted > 0 {
        db.search_index.invalidate();
    }
    if deleted > 0 || blobs_removed {
        let _ = app.emit("clipboard-change", ());
    }
    Ok(deleted)
}

/// Bind each id in a slice onto a sqlx query, regardless of its concrete type
/// (`query`, `query_scalar`, ...), returning the fully-bound query.
macro_rules! bind_all {
    ($query:expr, $ids:expr) => {{
        let mut query = $query;
        for id in $ids {
            query = query.bind(id);
        }
        query
    }};
}

/// Enforce retention: sweep clips past the keep-for window (and any item-count
/// overflow), plus anything the user soft-deleted.
///
/// SOU-244: an image clip that has recognized OCR text is *preserved* rather
/// than deleted when it ages out. Its full-resolution blob (the disk-heavy
/// `clip_images` row + `.cubby` file) is dropped, but the `clips` row keeps its
/// encrypted thumbnail and `ocr_text`, and is flagged `full_image_expired = 1`
/// so it stays browsable and searchable and is never re-swept by age/overflow.
/// Everything else — text/files clips, textless images, and explicit
/// soft-deletes — is still fully removed.
///
/// Returns `(rows_fully_deleted, disk_image_paths_to_unlink)`. Preserved images
/// contribute their disk path (the file is unlinked) but not to the delete count.
pub(crate) async fn enforce_retention_in_pool(
    pool: &SqlitePool,
    max_items: i64,
    auto_delete_days: i64,
) -> Result<(u64, Vec<String>), String> {
    // Images aged out by the keep-for window / overflow cap that still carry
    // OCR text. These are preserved (blob dropped, row + thumbnail + text kept),
    // not deleted. `full_image_expired = 0` keeps already-preserved clips out of
    // this set so they're processed at most once. Age uses last_accessed so a
    // just-imported live original is not swept by the dest keep-for window
    // while created_at (the visible history date) stays the source date.
    let preserve_query = r#"
        SELECT uuid FROM clips
        WHERE is_pinned = 0
          AND is_deleted = 0
          AND clip_type = 'image'
          AND full_image_expired = 0
          AND ocr_status = 'completed'
          AND ocr_text IS NOT NULL
          AND (
              (? > 0 AND last_accessed < datetime('now', '-' || ? || ' days'))
              OR (? > 0 AND uuid IN (
                  SELECT uuid FROM clips
                  WHERE is_deleted = 0 AND is_pinned = 0
                  ORDER BY created_at DESC
                  LIMIT -1 OFFSET ?
              ))
          )
    "#;

    // Clips to remove entirely. Soft-deletes always qualify. Age/overflow
    // sweeps skip clips already preserved by SOU-244 (`full_image_expired = 0`),
    // so a preserved thumbnail is never later hard-deleted by the age branch;
    // an explicit soft-delete of one still wipes it.
    let delete_query = r#"
        SELECT uuid FROM clips
        WHERE is_pinned = 0 AND (
            is_deleted = 1
            OR (full_image_expired = 0 AND (
                (? > 0 AND CASE WHEN clip_type = 'image' THEN last_accessed ELSE created_at END < datetime('now', '-' || ? || ' days'))
                OR (? > 0 AND uuid IN (
                    SELECT uuid FROM clips
                    WHERE is_deleted = 0 AND is_pinned = 0
                    ORDER BY created_at DESC
                    LIMIT -1 OFFSET ?
                ))
            ))
        )
    "#;

    let mut transaction = pool.begin().await.map_err(|error| error.to_string())?;

    let preserve: Vec<String> = bind_retention(
        sqlx::query_scalar(preserve_query),
        max_items,
        auto_delete_days,
    )
    .fetch_all(&mut *transaction)
    .await
    .map_err(|error| error.to_string())?;

    // Drop the full-resolution blobs of preserved images and flag the rows. Do
    // this before selecting the delete set so the flag excludes them from the
    // age/overflow branch below.
    let mut preserved_paths: Vec<String> = Vec::new();
    if !preserve.is_empty() {
        let placeholders = placeholders(preserve.len());
        let select_paths =
            format!("SELECT file_path FROM clip_images WHERE clip_uuid IN ({placeholders})");
        let delete_images = format!("DELETE FROM clip_images WHERE clip_uuid IN ({placeholders})");
        let flag_clips =
            format!("UPDATE clips SET full_image_expired = 1 WHERE uuid IN ({placeholders})");

        let paths: Vec<Option<String>> = bind_all!(sqlx::query_scalar(&select_paths), &preserve)
            .fetch_all(&mut *transaction)
            .await
            .map_err(|error| error.to_string())?;
        preserved_paths = paths.into_iter().flatten().collect();

        bind_all!(sqlx::query(&delete_images), &preserve)
            .execute(&mut *transaction)
            .await
            .map_err(|error| error.to_string())?;
        bind_all!(sqlx::query(&flag_clips), &preserve)
            .execute(&mut *transaction)
            .await
            .map_err(|error| error.to_string())?;
    }

    let candidates: Vec<String> = bind_retention(
        sqlx::query_scalar(delete_query),
        max_items,
        auto_delete_days,
    )
    .fetch_all(&mut *transaction)
    .await
    .map_err(|error| error.to_string())?;

    let mut deleted = 0u64;
    let mut deleted_paths: Vec<String> = Vec::new();
    if !candidates.is_empty() {
        let placeholders = placeholders(candidates.len());
        let select_paths =
            format!("SELECT file_path FROM clip_images WHERE clip_uuid IN ({placeholders})");
        let delete_images = format!("DELETE FROM clip_images WHERE clip_uuid IN ({placeholders})");
        let delete_clips = format!("DELETE FROM clips WHERE uuid IN ({placeholders})");

        let paths: Vec<Option<String>> = bind_all!(sqlx::query_scalar(&select_paths), &candidates)
            .fetch_all(&mut *transaction)
            .await
            .map_err(|error| error.to_string())?;
        deleted_paths = paths.into_iter().flatten().collect();

        bind_all!(sqlx::query(&delete_images), &candidates)
            .execute(&mut *transaction)
            .await
            .map_err(|error| error.to_string())?;
        deleted = bind_all!(sqlx::query(&delete_clips), &candidates)
            .execute(&mut *transaction)
            .await
            .map_err(|error| error.to_string())?
            .rows_affected();
    }

    transaction
        .commit()
        .await
        .map_err(|error| error.to_string())?;

    // Preserved and deleted images both leave a disk blob to unlink.
    deleted_paths.extend(preserved_paths);
    Ok((deleted, deleted_paths))
}

fn placeholders(count: usize) -> String {
    std::iter::repeat_n("?", count)
        .collect::<Vec<_>>()
        .join(",")
}

/// Bind the four retention parameters (age window twice, overflow cap twice) in
/// the order the retention `WHERE` clauses expect.
fn bind_retention<'q, O>(
    query: sqlx::query::QueryScalar<'q, sqlx::Sqlite, O, sqlx::sqlite::SqliteArguments<'q>>,
    max_items: i64,
    auto_delete_days: i64,
) -> sqlx::query::QueryScalar<'q, sqlx::Sqlite, O, sqlx::sqlite::SqliteArguments<'q>> {
    query
        .bind(auto_delete_days)
        .bind(auto_delete_days)
        .bind(max_items)
        .bind(max_items.max(0))
}

#[tauri::command]
pub async fn clear_unpinned_clips(db: tauri::State<'_, Arc<Database>>) -> Result<u64, String> {
    let (deleted, image_paths) = clear_clips_in_pool(&db.pool, true).await?;
    remove_clip_image_files(&db.image_dir, image_paths);
    crate::clipboard::reset_capture_dedup();
    if deleted > 0 {
        db.search_index.invalidate();
    }
    Ok(deleted)
}

#[tauri::command]
pub async fn clear_all_clips(db: tauri::State<'_, Arc<Database>>) -> Result<(), String> {
    let (_, image_paths) = clear_clips_in_pool(&db.pool, false).await?;
    remove_clip_image_files(&db.image_dir, image_paths);
    crate::clipboard::reset_capture_dedup();
    db.search_index.invalidate();
    Ok(())
}

#[tauri::command]
pub async fn remove_duplicate_clips(db: tauri::State<'_, Arc<Database>>) -> Result<i64, String> {
    remove_duplicate_clips_in_database(&db).await
}

/// Hard-delete unpinned clips that are not the oldest visible copy of their
/// hash. After `idx_clips_hash_unique` that is mostly unpinned soft-deletes.
///
/// `clip_images` cascades with the clip row, so the file paths have to be
/// collected first — the same order retention and hard-delete already use.
async fn remove_duplicate_clips_in_database(db: &Database) -> Result<i64, String> {
    let pool = &db.pool;
    let duplicate_filter = r#"
        is_pinned = 0
          AND id NOT IN (
            SELECT MIN(id)
            FROM clips
            WHERE is_deleted = 0
            GROUP BY content_hash
          )
    "#;
    let select_paths = format!(
        "SELECT file_path FROM clip_images WHERE clip_uuid IN (SELECT uuid FROM clips WHERE {duplicate_filter})"
    );
    let delete_clips = format!("DELETE FROM clips WHERE {duplicate_filter}");

    let mut transaction = pool.begin().await.map_err(|e| e.to_string())?;
    let image_paths: Vec<Option<String>> = sqlx::query_scalar(&select_paths)
        .fetch_all(&mut *transaction)
        .await
        .map_err(|e| e.to_string())?;
    let deleted = sqlx::query(&delete_clips)
        .execute(&mut *transaction)
        .await
        .map_err(|e| e.to_string())?
        .rows_affected();
    transaction.commit().await.map_err(|e| e.to_string())?;

    remove_clip_image_files(
        &db.image_dir,
        image_paths
            .into_iter()
            .flatten()
            .filter(|path| !path.is_empty())
            .collect(),
    );

    if deleted > 0 {
        crate::clipboard::reset_capture_dedup();
        db.search_index.invalidate();
    }

    Ok(deleted as i64)
}

#[tauri::command]
pub async fn refresh_window(app: AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("main") {
        let win_for_show = win.clone();
        crate::animate_window_hide(
            &win,
            Some(Box::new(move || {
                crate::position_window_near_cursor(&win_for_show);
            })),
        );
    }
    Ok(())
}

/// Label of the dedicated History window (SOU-582). The compact flyout stays
/// `main`; this is the roomy, resizable surface that has space for a preview
/// pane and the filters the flyout can't fit.
pub const HISTORY_WINDOW_LABEL: &str = "history";

/// Open the History window, or focus it if it is already open. Lives in Rust so
/// the tray menu and the flyout button share one implementation.
pub fn show_history_window(app: &AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(HISTORY_WINDOW_LABEL) {
        if let Err(e) = window.unminimize() {
            log::warn!("Failed to unminimize the history window: {:?}", e);
        }
        window.show().map_err(|e| e.to_string())?;
        window.set_focus().map_err(|e| e.to_string())?;
        return Ok(());
    }

    tauri::WebviewWindowBuilder::new(
        app,
        HISTORY_WINDOW_LABEL,
        tauri::WebviewUrl::App("index.html?window=history".into()),
    )
    .title("Cubby History")
    .inner_size(1040.0, 700.0)
    .min_inner_size(760.0, 460.0)
    .resizable(true)
    // Own title bar, same as the settings window.
    .decorations(false)
    .transparent(false)
    .center()
    .build()
    .map_err(|e| e.to_string())?;

    Ok(())
}

#[tauri::command]
pub async fn open_history_window(app: AppHandle) -> Result<(), String> {
    show_history_window(&app)
}

/// Label of the pop-out image viewer. One window, reused: opening a different
/// clip navigates the existing viewer rather than piling up windows.
pub const IMAGE_WINDOW_LABEL: &str = "image";

/// Open a screenshot in its own window, big enough to read it. The History
/// window's preview pane can only ever show a 2862px screenshot at a fraction
/// of its captured size; this is the surface where the pixels — and the text
/// selection over them — actually have room.
#[tauri::command]
pub async fn open_image_window(app: AppHandle, id: String) -> Result<(), String> {
    let url = format!("index.html?window=image&clip={id}");

    if let Some(window) = app.get_webview_window(IMAGE_WINDOW_LABEL) {
        // Re-point the existing viewer at the newly chosen clip.
        window
            .eval(format!(
                "window.location.replace({});",
                serde_json::to_string(&url).map_err(|e| e.to_string())?
            ))
            .map_err(|e| e.to_string())?;
        if let Err(e) = window.unminimize() {
            log::warn!("Failed to unminimize the image window: {:?}", e);
        }
        window.show().map_err(|e| e.to_string())?;
        window.set_focus().map_err(|e| e.to_string())?;
        return Ok(());
    }

    // Open large: most of the work area, so a full-size screenshot needs little
    // or no panning. Falls back to a sane fixed size if the monitor is unknown.
    let (width, height) = app
        .primary_monitor()
        .ok()
        .flatten()
        .map(|monitor| {
            let size = monitor.size().to_logical::<f64>(monitor.scale_factor());
            ((size.width * 0.82).round(), (size.height * 0.86).round())
        })
        .unwrap_or((1280.0, 860.0));

    tauri::WebviewWindowBuilder::new(&app, IMAGE_WINDOW_LABEL, tauri::WebviewUrl::App(url.into()))
        .title("Cubby Image")
        .inner_size(width, height)
        .min_inner_size(520.0, 400.0)
        .resizable(true)
        .decorations(false)
        .transparent(false)
        .center()
        .build()
        .map_err(|e| e.to_string())?;

    Ok(())
}

/// Everything the History window's preview pane needs beyond the list row.
/// The list query returns a thumbnail for images and the flyout never needs
/// more, so the full-resolution blob and the recognized text are loaded only
/// when a clip is actually selected for preview.
#[derive(serde::Serialize)]
pub struct ClipDetails {
    /// Full text, or base64 of the full-resolution image.
    pub content: String,
    pub ocr_text: Option<String>,
    /// Retention dropped the full image (SOU-244), so `content` is the surviving
    /// thumbnail rather than the original.
    pub image_expired: bool,
    /// Word boxes for selecting text straight off the preview. None for text
    /// clips, and for images captured before word layouts were recorded — those
    /// still offer the whole recognized block via `ocr_text`.
    pub ocr_layout: Option<crate::models::OcrTextLayout>,
    /// The clip's note. Hidden list rows withhold this (a note can name the
    /// secret); reveal and the preview pane read it here with the payload.
    pub notes: Option<String>,
}

#[tauri::command]
pub async fn get_clip_details(
    id: String,
    db: tauri::State<'_, Arc<Database>>,
) -> Result<ClipDetails, String> {
    get_clip_details_in_database(db.inner(), &id).await
}

async fn get_clip_details_in_database(db: &Database, id: &str) -> Result<ClipDetails, String> {
    let clip: Option<Clip> = sqlx::query_as(r#"SELECT * FROM clips WHERE uuid = ?"#)
        .bind(id)
        .fetch_optional(&db.pool)
        .await
        .map_err(|e| e.to_string())?;
    let mut clip = clip.ok_or_else(|| format!("Clip {} not found", id))?;
    decrypt_clip_fields(db, &mut clip)?;

    let image_expired = clip.full_image_expired;
    let content = if clip.clip_type == "image" && !image_expired {
        // Bind the loaded blob first. Passing the load call straight as an
        // argument would hold `&mut clip` across `&clip.clip_type` in the same
        // call, which does not borrow-check. The branch already knows the type.
        let full = load_full_image_content(db, &mut clip).await?;
        crate::clip_list::details_item_content("image", &full)
    } else {
        // Text, or an image whose full blob was dropped by retention: the
        // surviving bytes (thumbnail for the latter) are what the pane shows.
        crate::clip_list::details_item_content(&clip.clip_type, &clip.content)
    };

    let ocr_layout = clip
        .ocr_words
        .as_deref()
        .filter(|_| clip.clip_type == "image")
        .and_then(ocr_text_layout);

    Ok(ClipDetails {
        content,
        ocr_text: clip.ocr_text.filter(|text| !text.trim().is_empty()),
        image_expired,
        ocr_layout,
        notes: clip.notes.filter(|text| !text.trim().is_empty()),
    })
}

/// One entry in the History window's source-app filter.
#[derive(serde::Serialize)]
pub struct SourceAppCount {
    pub name: String,
    pub count: usize,
}

/// Every source app present in the history, with a live count, most used first.
///
/// Counts come from the in-memory index rather than a `GROUP BY`: `source_app`
/// is encrypted with a random nonce, so two clips from the same app do not
/// share a ciphertext and SQL cannot group them. The index already holds the
/// decrypted view of every clip, so this needs no extra table and no second
/// copy of the capture path's app-name resolution.
#[tauri::command]
pub async fn get_source_apps(
    db: tauri::State<'_, Arc<Database>>,
) -> Result<Vec<SourceAppCount>, String> {
    db.search_index.ensure_ready(&db.pool, &db.crypto).await?;
    Ok(db
        .search_index
        .source_app_counts()
        .into_iter()
        .map(|(name, count)| SourceAppCount { name, count })
        .collect())
}

/// Put a text selection made on an image preview onto the clipboard. Uses the
/// same ignore-hash discipline as the other copy paths so Cubby's own capture
/// loop doesn't treat the write as a fresh copy and duplicate it.
#[tauri::command]
pub async fn copy_selected_text(text: String, window: tauri::WebviewWindow) -> Result<(), String> {
    if text.is_empty() {
        return Err("Nothing selected".to_string());
    }

    let _guard = crate::clipboard::CLIPBOARD_SYNC.lock().await;
    let mut hash_material = b"text\0".to_vec();
    hash_material.extend_from_slice(text.as_bytes());
    let content_hash = crate::clipboard::calculate_hash(&hash_material);
    crate::clipboard::set_ignore_hash(content_hash.clone());
    if let Err(error) = ClipboardContext::new()
        .and_then(|context| context.set(vec![ClipboardContent::Text(text.clone())]))
    {
        crate::clipboard::clear_ignore_hash_if_matches(&content_hash);
        return Err(format!("Failed to copy the selection: {error}"));
    }

    let _ = window.emit("clipboard-write", &text);
    Ok(())
}

#[tauri::command]
pub async fn focus_window(app: AppHandle, label: String) -> Result<(), String> {
    if let Some(window) = app.get_webview_window(&label) {
        if let Err(e) = window.unminimize() {
            log::warn!("Failed to unminimize window {}: {:?}", label, e);
        }
        if let Err(e) = window.show() {
            log::warn!("Failed to show window {}: {:?}", label, e);
        }
        if let Err(e) = window.set_focus() {
            log::warn!("Failed to focus window {}: {:?}", label, e);
        }

        Ok(())
    } else {
        Err(format!("Window {} not found", label))
    }
}

#[tauri::command]
pub async fn pick_file(app: AppHandle) -> Result<String, String> {
    use tauri_plugin_dialog::DialogExt;

    let file_path = app
        .dialog()
        .file()
        .add_filter("Executables", &["exe"])
        .blocking_pick_file();

    match file_path {
        Some(path) => Ok(path.to_string()),
        None => Err("No file selected".to_string()),
    }
}

/// Where to save a new backup bundle. Separate from `pick_file` because this
/// one is a save dialog, not an open dialog.
#[tauri::command]
pub async fn pick_backup_save_path(app: AppHandle) -> Result<String, String> {
    use tauri_plugin_dialog::DialogExt;

    let suggested = format!(
        "cubby-backup-{}.cubbybak",
        chrono::Local::now().format("%Y-%m-%d")
    );
    match app
        .dialog()
        .file()
        .add_filter("Cubby backup", &["cubbybak"])
        .set_file_name(&suggested)
        .blocking_save_file()
    {
        Some(path) => crate::path_grant::grant_picker_path(
            crate::path_grant::PathGrantPurpose::BackupSave,
            path.to_string(),
        ),
        None => Err("No location selected".to_string()),
    }
}

#[tauri::command]
pub async fn pick_backup_file(app: AppHandle) -> Result<String, String> {
    use tauri_plugin_dialog::DialogExt;

    match app
        .dialog()
        .file()
        .add_filter("Cubby backup", &["cubbybak"])
        .blocking_pick_file()
    {
        Some(path) => crate::path_grant::grant_picker_path(
            crate::path_grant::PathGrantPurpose::BackupOpen,
            path.to_string(),
        ),
        None => Err("No file selected".to_string()),
    }
}

#[tauri::command]
pub async fn export_backup(
    path: String,
    passphrase: String,
    db: tauri::State<'_, Arc<Database>>,
) -> Result<usize, String> {
    crate::path_grant::export_granted_backup(db.inner(), path, passphrase).await
}

#[tauri::command]
pub async fn import_backup(
    path: String,
    passphrase: String,
    dry_run: bool,
    db: tauri::State<'_, Arc<Database>>,
) -> Result<crate::backup::BackupImportResult, String> {
    crate::path_grant::import_granted_backup(db.inner(), path, passphrase, dry_run).await
}

#[tauri::command]
pub async fn pick_ditto_database(app: AppHandle) -> Result<String, String> {
    use tauri_plugin_dialog::DialogExt;

    let mut dialog = app.dialog().file().add_filter("Ditto database", &["db"]);
    if let Ok(appdata) = std::env::var("APPDATA") {
        let default_dir = std::path::Path::new(&appdata).join("Ditto");
        if default_dir.exists() {
            dialog = dialog.set_directory(default_dir);
        }
    }

    match dialog.blocking_pick_file() {
        Some(path) => crate::path_grant::grant_picker_path(
            crate::path_grant::PathGrantPurpose::DittoOpen,
            path.to_string(),
        ),
        None => Err("No file selected".to_string()),
    }
}

#[tauri::command]
pub fn get_paste_context(
    settings: tauri::State<'_, Arc<crate::settings_manager::SettingsManager>>,
) -> crate::paste_engine::PasteContext {
    crate::paste_engine::paste_context(settings.get().remote_paste_mode)
}

#[tauri::command]
pub fn get_system_accent_color() -> Result<serde_json::Value, String> {
    #[cfg(target_os = "windows")]
    {
        use windows::UI::ViewManagement::{UIColorType, UISettings};

        let settings = UISettings::new().map_err(|error| error.to_string())?;
        let color = settings
            .GetColorValue(UIColorType::Accent)
            .map_err(|error| error.to_string())?;

        Ok(serde_json::json!({
            "red": color.R,
            "green": color.G,
            "blue": color.B,
            "alpha": color.A,
        }))
    }

    #[cfg(not(target_os = "windows"))]
    {
        Err("System accent color is only available on Windows".to_string())
    }
}

#[tauri::command]
pub async fn import_from_ditto(
    db_path: String,
    dry_run: bool,
    db: tauri::State<'_, Arc<Database>>,
) -> Result<crate::ditto_import::DittoImportResult, String> {
    let result = crate::path_grant::import_granted_ditto(&db, db_path, dry_run).await?;
    if !dry_run && result.imported > 0 {
        db.search_index.invalidate();
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::{
        build_ocr_highlights, build_ocr_match, clear_clips_in_pool, clipboard_contents_for_restore,
        delete_folder_in_pool, directory_size_bytes, enforce_retention_in_pool, excluded_log_dir,
        get_clip_details_in_database, get_clips_in_database, get_clips_request_log,
        history_disk_bytes, load_recognized_text, mark_clip_used, migrate_clip_format_model,
        migrate_encrypted_storage, ocr_text_layout, remove_clip_image_files,
        remove_duplicate_clips_in_database, restore_hash_material, search_clips_in_database,
        set_clip_notes_in_database, set_clip_ocr_text_in_database, source_app_filter_log_state,
        toggle_clip_hidden_in_pool, toggle_clip_pin_in_pool, update_clip_text_in_database,
        ClipboardContent, NOTE_CHAR_LIMIT, OCR_SNIPPET_CHAR_LIMIT,
    };
    use crate::clipboard::CapturedFormat;
    use crate::database::Database;
    use crate::models::Clip;
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
    use sqlx::sqlite::SqlitePoolOptions;
    use std::sync::Arc;

    async fn test_database() -> Database {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory database should open");
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&pool)
            .await
            .expect("foreign keys should be enabled in tests");
        let database = Database {
            pool,
            crypto: Arc::new(crate::crypto::CryptoManager::ephemeral()),
            image_dir: std::env::temp_dir().join(format!("cubby-test-{}", uuid::Uuid::new_v4())),
            search_index: Arc::new(crate::search_index::SearchIndex::default()),
        };
        database.migrate().await.expect("migration should succeed");
        database
    }

    struct SearchFixture<'a> {
        id: &'a str,
        clip_type: &'a str,
        content: &'a str,
        preview: &'a str,
        ocr: Option<&'a str>,
        folder_id: Option<i64>,
        pinned: bool,
        created_at: &'a str,
        source_app: Option<&'a str>,
    }

    impl<'a> SearchFixture<'a> {
        /// A plain unpinned text clip; tests override only what they exercise.
        fn text(id: &'a str, content: &'a str, created_at: &'a str) -> Self {
            Self {
                id,
                clip_type: "text",
                content,
                preview: content,
                ocr: None,
                folder_id: None,
                pinned: false,
                created_at,
                source_app: None,
            }
        }

        fn with_app(mut self, app: &'a str) -> Self {
            self.source_app = Some(app);
            self
        }
    }

    async fn insert_search_clip(database: &Database, fixture: SearchFixture<'_>) {
        let encrypted_content = database.crypto.encrypt(fixture.content.as_bytes()).unwrap();
        let encrypted_preview = database.crypto.encrypt_text(fixture.preview).unwrap();
        let encrypted_ocr = fixture
            .ocr
            .map(|text| database.crypto.encrypt_text(text).unwrap());
        let encrypted_source_app = database
            .crypto
            .encrypt_optional_text(fixture.source_app)
            .unwrap();
        sqlx::query(
            r#"
            INSERT INTO clips (
                uuid, clip_type, content, text_preview, content_hash,
                folder_id, is_pinned, created_at, ocr_text, source_app
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(fixture.id)
        .bind(fixture.clip_type)
        .bind(encrypted_content)
        .bind(encrypted_preview)
        .bind(format!("hash-{}", fixture.id))
        .bind(fixture.folder_id)
        .bind(fixture.pinned)
        .bind(fixture.created_at)
        .bind(encrypted_ocr)
        .bind(encrypted_source_app)
        .execute(&database.pool)
        .await
        .unwrap();
    }

    async fn created_at_of(database: &Database, uuid: &str) -> String {
        sqlx::query_scalar("SELECT created_at FROM clips WHERE uuid = ?")
            .bind(uuid)
            .fetch_one(&database.pool)
            .await
            .expect("clip should exist")
    }

    /// A failed clipboard write must leave history ordering alone. Bumping
    /// `created_at` anyway moved the clip to the top and restarted its
    /// retention clock for a paste the user never received.
    #[tokio::test]
    async fn failed_restore_does_not_bump_the_clip() {
        let database = test_database().await;
        insert_search_clip(
            &database,
            SearchFixture::text("stale", "never pasted", "2026-03-01 12:00:00"),
        )
        .await;

        mark_clip_used(&database.pool, "stale", false).await;

        assert_eq!(
            created_at_of(&database, "stale").await,
            "2026-03-01 12:00:00",
            "a failed clipboard write must not reorder history"
        );
    }

    #[tokio::test]
    async fn successful_restore_bumps_the_clip() {
        let database = test_database().await;
        insert_search_clip(
            &database,
            SearchFixture::text("used", "actually pasted", "2026-03-01 12:00:00"),
        )
        .await;

        mark_clip_used(&database.pool, "used", true).await;

        assert_ne!(
            created_at_of(&database, "used").await,
            "2026-03-01 12:00:00",
            "a successful paste must count as recent use"
        );
    }

    async fn corrupt_clip_content(database: &Database, id: &str) {
        sqlx::query("UPDATE clips SET content = ? WHERE uuid = ?")
            .bind(b"not-cub1".as_slice())
            .bind(id)
            .execute(&database.pool)
            .await
            .unwrap();
    }

    async fn corrupt_clip_preview(database: &Database, id: &str) {
        sqlx::query("UPDATE clips SET text_preview = ? WHERE uuid = ?")
            .bind("not-cub1")
            .bind(id)
            .execute(&database.pool)
            .await
            .unwrap();
    }

    async fn corrupt_clip_source_app(database: &Database, id: &str) {
        sqlx::query("UPDATE clips SET source_app = ? WHERE uuid = ?")
            .bind("not-cub1")
            .bind(id)
            .execute(&database.pool)
            .await
            .unwrap();
    }

    /// `count` text clips one minute apart, all matching the query "pageable".
    /// Returns their ids in listing order: newest first.
    async fn seed_paging_clips(database: &Database, count: usize) -> Vec<String> {
        let mut ids = Vec::with_capacity(count);
        for index in 0..count {
            let id = format!("page-{index:02}");
            let created_at = format!("2026-07-01 12:{index:02}:00");
            insert_search_clip(
                database,
                SearchFixture::text(&id, "pageable body", &created_at),
            )
            .await;
            ids.push(id);
        }
        ids.reverse();
        ids
    }

    async fn paged_ids(database: &Database, limit: i64, offset: i64) -> Vec<String> {
        get_clips_in_database(None, limit, offset, None, None, None, None, None, database)
            .await
            .expect("listing must survive an unreadable clip")
            .into_iter()
            .map(|clip| clip.id)
            .collect()
    }

    async fn searched_ids(database: &Database, limit: i64, offset: i64) -> Vec<String> {
        search_clips_in_database(
            "pageable".into(),
            None,
            limit,
            offset,
            None,
            None,
            None,
            None,
            None,
            database,
        )
        .await
        .expect("search must survive an unreadable hit")
        .into_iter()
        .map(|clip| clip.id)
        .collect()
    }

    async fn paged_after(database: &Database, limit: i64, after_id: &str) -> Vec<String> {
        super::get_clips_paged(
            None,
            limit,
            0,
            None,
            None,
            None,
            None,
            None,
            Some(after_id.to_string()),
            database,
        )
        .await
        .expect("listing must survive an unreadable clip")
        .into_iter()
        .map(|clip| clip.id)
        .collect()
    }

    async fn searched_after(database: &Database, limit: i64, after_id: &str) -> Vec<String> {
        super::search_clips_paged(
            "pageable".into(),
            None,
            limit,
            0,
            None,
            None,
            None,
            None,
            None,
            Some(after_id.to_string()),
            database,
        )
        .await
        .expect("search must survive an unreadable hit")
        .into_iter()
        .map(|clip| clip.id)
        .collect()
    }

    /// Three clips over three days from two apps, newest last.
    async fn seed_filter_clips(database: &Database) {
        insert_search_clip(
            database,
            SearchFixture::text("day-one", "alpha one", "2026-03-01 12:00:00").with_app("code.exe"),
        )
        .await;
        insert_search_clip(
            database,
            SearchFixture::text("day-two", "alpha two", "2026-03-02 12:00:00")
                .with_app("chrome.exe"),
        )
        .await;
        insert_search_clip(
            database,
            SearchFixture::text("day-three", "alpha three", "2026-03-03 12:00:00")
                .with_app("code.exe"),
        )
        .await;
    }

    async fn listed_ids(
        database: &Database,
        date_from: Option<&str>,
        date_to: Option<&str>,
        source_app: Option<&str>,
    ) -> Vec<String> {
        get_clips_in_database(
            None,
            50,
            0,
            Some(true),
            None,
            date_from.map(str::to_string),
            date_to.map(str::to_string),
            source_app.map(str::to_string),
            database,
        )
        .await
        .unwrap()
        .into_iter()
        .map(|clip| clip.id)
        .collect()
    }

    #[tokio::test]
    async fn paging_is_stable_when_clips_share_a_timestamp() {
        let database = test_database().await;
        // created_at has one-second resolution, so a burst of copies lands on
        // the same value. Ten of them, paged two at a time.
        for index in 0..10 {
            insert_search_clip(
                &database,
                SearchFixture::text(
                    &format!("tie-{index}"),
                    "alpha burst",
                    "2026-04-01 09:00:00",
                )
                .with_app("code.exe"),
            )
            .await;
        }

        // Walk each paging path two rows at a time and require the pages, laid
        // end to end, to reproduce the unpaged listing exactly: same rows, same
        // order, nothing repeated or skipped.
        //
        // Note this does not *reproduce* the tie-break defect. SQLite leaves the
        // order of tied rows unspecified but happens to be stable here, so the
        // bug is not summonable on demand; the `uuid` key removes the
        // reliance on that. What this does guard is the invariant itself, which
        // is what any future change to ordering or paging would break.
        for path in ["plain", "source_app", "search"] {
            let mut seen: Vec<String> = Vec::new();
            for page in 0..5 {
                let ids: Vec<String> = match path {
                    "search" => search_clips_in_database(
                        "alpha".into(),
                        None,
                        2,
                        page * 2,
                        None,
                        None,
                        None,
                        None,
                        None,
                        &database,
                    )
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|clip| clip.id)
                    .collect(),
                    "source_app" => listed_page(&database, 2, page * 2, Some("code.exe")).await,
                    _ => listed_page(&database, 2, page * 2, None).await,
                };
                seen.extend(ids);
            }

            let unpaged: Vec<String> = match path {
                "search" => search_clips_in_database(
                    "alpha".into(),
                    None,
                    50,
                    0,
                    None,
                    None,
                    None,
                    None,
                    None,
                    &database,
                )
                .await
                .unwrap()
                .into_iter()
                .map(|clip| clip.id)
                .collect(),
                "source_app" => listed_page(&database, 50, 0, Some("code.exe")).await,
                _ => listed_page(&database, 50, 0, None).await,
            };

            assert_eq!(unpaged.len(), 10, "{path} should list every clip");
            assert_eq!(
                seen, unpaged,
                "{path} paging disagreed with the full listing"
            );
        }
    }

    async fn listed_page(
        database: &Database,
        limit: i64,
        offset: i64,
        source_app: Option<&str>,
    ) -> Vec<String> {
        get_clips_in_database(
            None,
            limit,
            offset,
            Some(true),
            None,
            None,
            None,
            source_app.map(str::to_string),
            database,
        )
        .await
        .unwrap()
        .into_iter()
        .map(|clip| clip.id)
        .collect()
    }

    #[tokio::test]
    async fn editing_a_clip_replaces_its_text_hash_and_stale_rich_formats() {
        let database = test_database().await;
        insert_search_clip(
            &database,
            SearchFixture::text("editable", "teh quick brown fox", "2026-06-01 09:00:00"),
        )
        .await;
        // A rich capture: the html describes the *old* text.
        sqlx::query(
            "INSERT INTO clip_formats (clip_uuid, format, content) VALUES ('editable', 'html', ?)",
        )
        .bind(
            database
                .crypto
                .encrypt(b"<b>teh quick brown fox</b>")
                .unwrap(),
        )
        .execute(&database.pool)
        .await
        .unwrap();

        let before: String = sqlx::query_scalar("SELECT content_hash FROM clips WHERE uuid = ?")
            .bind("editable")
            .fetch_one(&database.pool)
            .await
            .unwrap();

        update_clip_text_in_database(&database, "editable", "the quick brown fox")
            .await
            .expect("edit should apply");

        let details = get_clip_details_in_database(&database, "editable")
            .await
            .unwrap();
        assert_eq!(details.content, "the quick brown fox");

        // The hash follows the content, so dedup keeps working against what the
        // clip now holds rather than what it used to.
        let after: String = sqlx::query_scalar("SELECT content_hash FROM clips WHERE uuid = ?")
            .bind("editable")
            .fetch_one(&database.pool)
            .await
            .unwrap();
        assert_ne!(before, after);
        let expected = database
            .crypto
            .keyed_hash(&crate::clipboard::build_clip_hash_material(
                "text",
                b"the quick brown fox",
                std::iter::empty::<(&str, &[u8])>(),
            ));
        assert_eq!(after, expected);

        // The stale rich format is gone; keeping it would paste the pre-edit
        // text into anything that prefers html, making the edit look ignored.
        let formats: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM clip_formats WHERE clip_uuid = 'editable'")
                .fetch_one(&database.pool)
                .await
                .unwrap();
        assert_eq!(formats, 0);

        // Search follows the edit rather than the original wording.
        database
            .search_index
            .ensure_ready(&database.pool, &database.crypto)
            .await
            .unwrap();
        update_clip_text_in_database(&database, "editable", "the quick brown fox")
            .await
            .unwrap();
        assert!(database.search_index.matches("quick").contains("editable"));
        assert!(database.search_index.matches("teh").is_empty());
    }

    /// SBS-994: a ~450-character edit used to persist 450 characters of
    /// `text_preview`. Every preview-only list and search then shipped that
    /// prefix into the WebView. Capture stores 200; the edit path must too.
    #[tokio::test]
    async fn editing_a_long_clip_stores_the_capture_preview_limit() {
        let database = test_database().await;
        insert_search_clip(
            &database,
            SearchFixture::text("editable", "short original", "2026-06-01 09:00:00"),
        )
        .await;

        let prefix = "a".repeat(crate::clip_list::TEXT_PREVIEW_CHAR_LIMIT);
        let secret = "UNIQUE-SBS-994-EDITED-TAIL-SHOULD-NOT-SHIP";
        let edited = format!("{prefix}{secret}");
        update_clip_text_in_database(&database, "editable", &edited)
            .await
            .expect("edit should apply");

        let stored: String = sqlx::query_scalar("SELECT text_preview FROM clips WHERE uuid = ?")
            .bind("editable")
            .fetch_one(&database.pool)
            .await
            .unwrap();
        let decrypted = database.crypto.decrypt_text(&stored).unwrap();
        assert_eq!(
            decrypted.chars().count(),
            crate::clip_list::TEXT_PREVIEW_CHAR_LIMIT
        );
        assert_eq!(decrypted, prefix);
        assert!(
            !decrypted.contains(secret),
            "stored text_preview must not keep the tail past the capture limit"
        );

        let listed =
            get_clips_in_database(None, 10, 0, Some(true), None, None, None, None, &database)
                .await
                .unwrap();
        let row = listed
            .iter()
            .find(|clip| clip.id == "editable")
            .expect("edited clip should list");
        assert!(
            row.content.is_empty(),
            "preview_only must withhold the body"
        );
        assert_eq!(row.preview, prefix);
        assert!(!row.preview.contains(secret));

        let searched = search_clips_in_database(
            "UNIQUE-SBS-994".to_string(),
            None,
            10,
            0,
            Some(true),
            None,
            None,
            None,
            None,
            &database,
        )
        .await
        .unwrap();
        let search_row = searched
            .iter()
            .find(|clip| clip.id == "editable")
            .expect("the full body is still searchable");
        assert!(search_row.content.is_empty());
        assert_eq!(search_row.preview, prefix);
        assert!(!search_row.preview.contains(secret));
    }

    #[tokio::test]
    async fn editing_refuses_an_image_and_an_unknown_clip() {
        let database = test_database().await;
        sqlx::query(
            "INSERT INTO clips (uuid, clip_type, content, text_preview, content_hash) VALUES ('shot', 'image', ?, ?, 'h')",
        )
        .bind(database.crypto.encrypt(&[1, 2, 3]).unwrap())
        .bind(database.crypto.encrypt_text("Screenshot").unwrap())
        .execute(&database.pool)
        .await
        .unwrap();

        assert!(update_clip_text_in_database(&database, "shot", "nope")
            .await
            .unwrap_err()
            .contains("Image clips cannot be edited"));
        assert!(update_clip_text_in_database(&database, "missing", "nope")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn editing_a_rich_clip_to_an_existing_hash_is_rejected_without_mutation() {
        let database = test_database().await;
        insert_search_clip(
            &database,
            SearchFixture::text("editable", "original wording", "2026-06-01 09:00:00"),
        )
        .await;
        insert_search_clip(
            &database,
            SearchFixture::text("existing", "already stored", "2026-06-01 10:00:00"),
        )
        .await;

        let original_html = database.crypto.encrypt(b"<b>original wording</b>").unwrap();
        let original_rtf = database.crypto.encrypt(b"{\\rtf original}").unwrap();
        sqlx::query(
            "INSERT INTO clip_formats (clip_uuid, format, content) VALUES ('editable', 'html', ?), ('editable', 'rtf', ?)",
        )
        .bind(&original_html)
        .bind(&original_rtf)
        .execute(&database.pool)
        .await
        .unwrap();

        let existing_hash =
            database
                .crypto
                .keyed_hash(&crate::clipboard::build_clip_hash_material(
                    "text",
                    b"already stored",
                    std::iter::empty::<(&str, &[u8])>(),
                ));
        sqlx::query("UPDATE clips SET content_hash = ? WHERE uuid = 'existing'")
            .bind(&existing_hash)
            .execute(&database.pool)
            .await
            .unwrap();

        let before_text = get_clip_details_in_database(&database, "editable")
            .await
            .unwrap()
            .content;
        let before_hash: String =
            sqlx::query_scalar("SELECT content_hash FROM clips WHERE uuid = 'editable'")
                .fetch_one(&database.pool)
                .await
                .unwrap();

        let error = update_clip_text_in_database(&database, "editable", "already stored")
            .await
            .expect_err("a duplicate-hash edit must be rejected");
        assert!(
            error.to_lowercase().contains("already"),
            "expected a duplicate-target error, got: {error}"
        );

        let after = get_clip_details_in_database(&database, "editable")
            .await
            .unwrap();
        assert_eq!(after.content, before_text);
        assert_eq!(after.content, "original wording");
        let after_hash: String =
            sqlx::query_scalar("SELECT content_hash FROM clips WHERE uuid = 'editable'")
                .fetch_one(&database.pool)
                .await
                .unwrap();
        assert_eq!(after_hash, before_hash);

        let format_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM clip_formats WHERE clip_uuid = 'editable'")
                .fetch_one(&database.pool)
                .await
                .unwrap();
        assert_eq!(format_count, 2);
        let html: Vec<u8> = sqlx::query_scalar(
            "SELECT content FROM clip_formats WHERE clip_uuid = 'editable' AND format = 'html'",
        )
        .fetch_one(&database.pool)
        .await
        .unwrap();
        let rtf: Vec<u8> = sqlx::query_scalar(
            "SELECT content FROM clip_formats WHERE clip_uuid = 'editable' AND format = 'rtf'",
        )
        .fetch_one(&database.pool)
        .await
        .unwrap();
        assert_eq!(html, original_html);
        assert_eq!(rtf, original_rtf);
    }

    #[tokio::test]
    async fn a_corrected_reading_replaces_what_search_indexed() {
        let database = test_database().await;
        sqlx::query(
            "INSERT INTO clips (uuid, clip_type, content, text_preview, content_hash, ocr_text, ocr_status) VALUES ('shot', 'image', ?, ?, 'h', ?, 'completed')",
        )
        .bind(database.crypto.encrypt(&[1, 2, 3]).unwrap())
        .bind(database.crypto.encrypt_text("Screenshot").unwrap())
        // The engine misread a 1 as an l.
        .bind(database.crypto.encrypt_text("invoice AKlA9").unwrap())
        .execute(&database.pool)
        .await
        .unwrap();
        database
            .search_index
            .ensure_ready(&database.pool, &database.crypto)
            .await
            .unwrap();
        assert!(database.search_index.matches("AKlA9").contains("shot"));

        set_clip_ocr_text_in_database(&database, "shot", "  invoice AK1A9  ")
            .await
            .unwrap();

        // The correction is what search now knows, which is the whole reason
        // for writing it back rather than keeping it transient.
        assert!(database.search_index.matches("AK1A9").contains("shot"));
        assert!(database.search_index.matches("AKlA9").is_empty());
        assert_eq!(
            load_recognized_text(&database, "shot").await.unwrap(),
            "invoice AK1A9"
        );

        // Marked processed, so the background worker cannot overwrite it.
        let status: Option<String> =
            sqlx::query_scalar("SELECT ocr_status FROM clips WHERE uuid = 'shot'")
                .fetch_one(&database.pool)
                .await
                .unwrap();
        assert_eq!(status.as_deref(), Some("completed"));

        // Not stored in the clear.
        let raw: Option<String> =
            sqlx::query_scalar("SELECT ocr_text FROM clips WHERE uuid = 'shot'")
                .fetch_one(&database.pool)
                .await
                .unwrap();
        assert!(!raw.unwrap().contains("invoice"));
    }

    #[tokio::test]
    async fn a_corrected_reading_rewrites_drag_select_word_boxes() {
        // SBS-1010: saving a correction used to leave ocr_words as the engine
        // stored them, so drag-select and search highlights still copied the
        // misreading after Copy text already used the fix.
        let database = test_database().await;
        let layout = crate::ocr::OcrLayout {
            image_width: 100,
            image_height: 50,
            words: vec![crate::ocr::OcrWordBox {
                text: "htlps://exarnple.com".to_string(),
                x: 10.0,
                y: 5.0,
                width: 80.0,
                height: 10.0,
                line: Some(0),
            }],
        };
        sqlx::query(
            "INSERT INTO clips (uuid, clip_type, content, text_preview, content_hash, ocr_text, ocr_words, ocr_status) VALUES ('shot', 'image', ?, ?, 'h', ?, ?, 'completed')",
        )
        .bind(database.crypto.encrypt(&[1, 2, 3]).unwrap())
        .bind(database.crypto.encrypt_text("Screenshot").unwrap())
        .bind(database.crypto.encrypt_text("htlps://exarnple.com").unwrap())
        .bind(
            database
                .crypto
                .encrypt_text(&serde_json::to_string(&layout).unwrap())
                .unwrap(),
        )
        .execute(&database.pool)
        .await
        .unwrap();

        set_clip_ocr_text_in_database(&database, "shot", "https://example.com")
            .await
            .unwrap();

        let details = get_clip_details_in_database(&database, "shot")
            .await
            .expect("details should load");
        assert_eq!(details.ocr_text.as_deref(), Some("https://example.com"));
        let words = details.ocr_layout.expect("layout should remain").words;
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].text, "https://example.com");
        assert_eq!(words[0].x, 0.1);
        assert_eq!(words[0].width, 0.8);

        // Not stored in the clear.
        let raw_words: Option<String> =
            sqlx::query_scalar("SELECT ocr_words FROM clips WHERE uuid = 'shot'")
                .fetch_one(&database.pool)
                .await
                .unwrap();
        assert!(!raw_words.unwrap().contains("example"));

        // Clearing the assembled block must drop the boxes too.
        set_clip_ocr_text_in_database(&database, "shot", "   ")
            .await
            .unwrap();
        let cleared = get_clip_details_in_database(&database, "shot")
            .await
            .expect("details should load");
        assert!(cleared.ocr_text.is_none());
        assert!(cleared.ocr_layout.is_none());
    }

    #[tokio::test]
    async fn correcting_recognized_text_refuses_a_non_image() {
        let database = test_database().await;
        insert_search_clip(
            &database,
            SearchFixture::text("plain", "just text", "2026-06-01 09:00:00"),
        )
        .await;
        assert!(set_clip_ocr_text_in_database(&database, "plain", "nope")
            .await
            .unwrap_err()
            .contains("image clips"));
        assert!(set_clip_ocr_text_in_database(&database, "missing", "nope")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn a_note_is_stored_encrypted_and_makes_its_clip_findable() {
        let database = test_database().await;
        insert_search_clip(
            &database,
            SearchFixture::text(
                "uuid-clip",
                "9f2c1b7e-40aa-4f11-b0d2-77c9e1f00a31",
                "2026-06-01 09:00:00",
            ),
        )
        .await;
        database
            .search_index
            .ensure_ready(&database.pool, &database.crypto)
            .await
            .unwrap();

        // The whole point: a clip with no memorable text becomes findable by
        // what the user called it.
        assert!(database.search_index.matches("staging api key").is_empty());
        set_clip_notes_in_database(&database, "uuid-clip", "  staging api key  ")
            .await
            .unwrap();
        assert!(database
            .search_index
            .matches("staging api key")
            .contains("uuid-clip"));

        let found = search_clips_in_database(
            "staging api".into(),
            None,
            10,
            0,
            None,
            None,
            None,
            None,
            None,
            &database,
        )
        .await
        .unwrap();
        assert_eq!(found.len(), 1);
        // Trimmed on the way in, and handed back for the row to show.
        assert_eq!(found[0].notes.as_deref(), Some("staging api key"));

        // Stored encrypted, like every other clip field.
        let raw: Option<String> = sqlx::query_scalar("SELECT notes FROM clips WHERE uuid = ?")
            .bind("uuid-clip")
            .fetch_one(&database.pool)
            .await
            .unwrap();
        let raw = raw.expect("a note should be stored");
        assert!(
            !raw.contains("staging"),
            "the note must not sit in the clear"
        );

        // Clearing removes it rather than storing a blank, so "has a note"
        // stays a real distinction.
        set_clip_notes_in_database(&database, "uuid-clip", "   ")
            .await
            .unwrap();
        let cleared: Option<String> = sqlx::query_scalar("SELECT notes FROM clips WHERE uuid = ?")
            .bind("uuid-clip")
            .fetch_one(&database.pool)
            .await
            .unwrap();
        assert!(cleared.is_none());
        assert!(database.search_index.matches("staging api key").is_empty());

        assert!(set_clip_notes_in_database(&database, "missing", "x")
            .await
            .is_err());

        // Bounded for callers that are not the capped input. Counted in chars,
        // so a multi-byte note is measured the same way as an ASCII one.
        assert!(
            set_clip_notes_in_database(&database, "uuid-clip", &"a".repeat(NOTE_CHAR_LIMIT))
                .await
                .is_ok(),
            "a note at the limit should be accepted"
        );
        assert!(
            set_clip_notes_in_database(&database, "uuid-clip", &"a".repeat(NOTE_CHAR_LIMIT + 1))
                .await
                .is_err(),
            "a note past the limit should be refused"
        );
        assert!(
            set_clip_notes_in_database(&database, "uuid-clip", &"é".repeat(NOTE_CHAR_LIMIT))
                .await
                .is_ok(),
            "the limit counts characters, not bytes"
        );
    }

    #[tokio::test]
    async fn hidden_clips_ship_no_content_but_stay_listed_and_pastable() {
        let database = test_database().await;
        insert_search_clip(
            &database,
            SearchFixture::text("secret", "swordfish token 8821", "2026-05-01 09:00:00")
                .with_app("code.exe"),
        )
        .await;
        set_clip_notes_in_database(&database, "secret", "AWS root password")
            .await
            .unwrap();

        assert_eq!(
            listed_ids(&database, None, None, None).await,
            vec!["secret"]
        );

        let hidden = toggle_clip_hidden_in_pool(&database.pool, "secret")
            .await
            .expect("toggle should apply");
        assert!(hidden);

        let items =
            get_clips_in_database(None, 10, 0, Some(true), None, None, None, None, &database)
                .await
                .unwrap();
        assert_eq!(items.len(), 1, "a hidden clip is still listed");
        assert!(items[0].is_hidden);
        // The point of the feature: the secret is not in the payload at all, so
        // it cannot be read off the row or out of the renderer's memory.
        assert!(items[0].content.is_empty());
        assert!(items[0].preview.is_empty());
        assert!(
            items[0].notes.is_none(),
            "a note naming the secret must not ship on the hidden row"
        );

        // Still searchable, and the match snippet does not leak it either.
        let found = search_clips_in_database(
            "swordfish".into(),
            None,
            10,
            0,
            None,
            None,
            None,
            None,
            None,
            &database,
        )
        .await
        .unwrap();
        assert_eq!(found.len(), 1);
        assert!(found[0].is_hidden);
        assert!(found[0].content.is_empty());
        assert!(
            found[0].ocr_match.is_none(),
            "the match snippet is made of the hidden text"
        );

        // Revealing for the session still sees the real thing, which is also
        // what paste reads.
        let details = get_clip_details_in_database(&database, "secret")
            .await
            .unwrap();
        assert_eq!(details.content, "swordfish token 8821");
        assert_eq!(details.notes.as_deref(), Some("AWS root password"));

        assert!(!toggle_clip_hidden_in_pool(&database.pool, "secret")
            .await
            .unwrap());
        let items =
            get_clips_in_database(None, 10, 0, Some(true), None, None, None, None, &database)
                .await
                .unwrap();
        assert!(!items[0].is_hidden);
        // preview_only still withholds the body after unhide; the row shows
        // the stored preview, and get_clip_details remains the reveal path.
        assert!(items[0].content.is_empty());
        assert_eq!(items[0].preview, "swordfish token 8821");
    }

    #[tokio::test]
    async fn preview_only_list_withholds_full_text_but_details_still_return_it() {
        let database = test_database().await;
        // A unique suffix so we can prove the full body is absent, not just
        // that the preview happens to look similar.
        let secret = "UNIQUE-SBS-829-SECRET-TOKEN-8821-DO-NOT-SHIP";
        let full_body = format!("{}{secret}", "copied log line\n".repeat(200));
        assert!(
            full_body.len() > 2000,
            "the fixture must be large enough that preview cannot be the whole body"
        );
        insert_search_clip(
            &database,
            SearchFixture {
                id: "dump",
                clip_type: "text",
                content: &full_body,
                preview: "copied log line",
                ocr: None,
                folder_id: None,
                pinned: false,
                created_at: "2026-08-15 09:00:00",
                source_app: None,
            },
        )
        .await;
        insert_search_clip(
            &database,
            SearchFixture {
                id: "shot",
                clip_type: "image",
                content: "thumb-png-bytes",
                preview: "Screenshot",
                ocr: None,
                folder_id: None,
                pinned: false,
                created_at: "2026-08-15 08:00:00",
                source_app: None,
            },
        )
        .await;

        let preview_only =
            get_clips_in_database(None, 10, 0, Some(true), None, None, None, None, &database)
                .await
                .unwrap();
        let dump = preview_only
            .iter()
            .find(|item| item.id == "dump")
            .expect("the text dump should be listed");
        assert!(
            dump.content.is_empty(),
            "preview_only text rows must not ship the decrypted body"
        );
        assert_eq!(dump.preview, "copied log line");
        assert!(
            !dump.content.contains(secret),
            "the unique secret must not appear in list content"
        );
        assert!(
            !dump.preview.contains(secret),
            "the stored preview is the truncated list text, not the full dump"
        );

        let shot = preview_only
            .iter()
            .find(|item| item.id == "shot")
            .expect("the image should be listed");
        assert_eq!(
            shot.content,
            BASE64.encode(b"thumb-png-bytes"),
            "image rows still ship the thumbnail preview_only is meant to keep"
        );
        assert_eq!(shot.preview, "Screenshot");

        let full =
            get_clips_in_database(None, 10, 0, Some(false), None, None, None, None, &database)
                .await
                .unwrap();
        let dump_full = full
            .iter()
            .find(|item| item.id == "dump")
            .expect("the text dump should be listed");
        assert_eq!(dump_full.content, full_body);
        assert!(dump_full.content.contains(secret));

        let omitted = get_clips_in_database(None, 10, 0, None, None, None, None, None, &database)
            .await
            .unwrap();
        let dump_omitted = omitted
            .iter()
            .find(|item| item.id == "dump")
            .expect("the text dump should be listed");
        assert_eq!(
            dump_omitted.content, full_body,
            "omitting preview_only keeps today's full-content list payload"
        );

        toggle_clip_hidden_in_pool(&database.pool, "dump")
            .await
            .expect("hide should apply");
        let hidden =
            get_clips_in_database(None, 10, 0, Some(true), None, None, None, None, &database)
                .await
                .unwrap();
        let dump_hidden = hidden
            .iter()
            .find(|item| item.id == "dump")
            .expect("a hidden clip is still listed");
        assert!(dump_hidden.is_hidden);
        assert!(dump_hidden.content.is_empty());
        assert!(dump_hidden.preview.is_empty());

        let details = get_clip_details_in_database(&database, "dump")
            .await
            .unwrap();
        assert_eq!(details.content, full_body);
        assert!(details.content.contains(secret));
    }

    /// Search path for SBS-912. A >2 KB dump with a unique suffix must not
    /// appear in `search_clips` content, including when a caller explicitly
    /// asks for `previewOnly: false` — search has no caller that needs a body,
    /// and honoring that opt-out is the leak. Full body stays on
    /// get_clip_details. Do not fold this into the get_clips test above; that
    /// path was SBS-829.
    #[tokio::test]
    async fn preview_only_search_withholds_full_text() {
        let database = test_database().await;
        let secret = "UNIQUE-SBS-912-SECRET-TOKEN-8821-DO-NOT-SHIP";
        let full_body = format!("{}{secret}", "copied log line\n".repeat(200));
        assert!(
            full_body.len() > 2000,
            "the fixture must be large enough that preview cannot be the whole body"
        );
        insert_search_clip(
            &database,
            SearchFixture {
                id: "dump",
                clip_type: "text",
                content: &full_body,
                preview: "copied log line",
                ocr: None,
                folder_id: None,
                pinned: false,
                created_at: "2026-08-16 09:00:00",
                source_app: None,
            },
        )
        .await;
        insert_search_clip(
            &database,
            SearchFixture {
                id: "shot",
                clip_type: "image",
                content: "thumb-png-bytes",
                preview: "Screenshot",
                ocr: Some("copied log line on the screenshot"),
                folder_id: None,
                pinned: false,
                created_at: "2026-08-16 08:00:00",
                source_app: None,
            },
        )
        .await;

        // Some(false) is in the loop on purpose: search ignores the opt-out.
        for requested in [Some(true), None, Some(false)] {
            let found = search_clips_in_database(
                "copied log line".into(),
                None,
                10,
                0,
                requested,
                None,
                None,
                None,
                None,
                &database,
            )
            .await
            .unwrap();
            let dump = found
                .iter()
                .find(|item| item.id == "dump")
                .expect("the text dump should be a search hit");
            assert!(
                dump.content.is_empty(),
                "search text rows must not ship the decrypted body when preview_only={requested:?}"
            );
            assert_eq!(dump.preview, "copied log line");
            assert!(
                !dump.content.contains(secret),
                "the unique secret must not appear in search content"
            );
            assert!(
                !dump.preview.contains(secret),
                "the stored preview is the truncated list text, not the full dump"
            );

            let shot = found
                .iter()
                .find(|item| item.id == "shot")
                .expect("the image should be a search hit");
            assert_eq!(
                shot.content,
                BASE64.encode(b"thumb-png-bytes"),
                "image search rows still ship the thumbnail"
            );
            assert_eq!(shot.preview, "Screenshot");
            assert!(
                shot.ocr_match.is_some(),
                "visible image search still ships the OCR snippet"
            );
        }

        // The full body has exactly one path, and it is not search.
        let details = get_clip_details_in_database(&database, "dump")
            .await
            .expect("details should return the full body");
        assert_eq!(details.content, full_body);
        assert!(details.content.contains(secret));

        toggle_clip_hidden_in_pool(&database.pool, "dump")
            .await
            .expect("hide should apply");
        let hidden = search_clips_in_database(
            "copied log line".into(),
            None,
            10,
            0,
            Some(true),
            None,
            None,
            None,
            None,
            &database,
        )
        .await
        .unwrap();
        let dump_hidden = hidden
            .iter()
            .find(|item| item.id == "dump")
            .expect("a hidden clip is still searchable");
        assert!(dump_hidden.is_hidden);
        assert!(dump_hidden.content.is_empty());
        assert!(dump_hidden.preview.is_empty());

        let details = get_clip_details_in_database(&database, "dump")
            .await
            .unwrap();
        assert_eq!(details.content, full_body);
        assert!(details.content.contains(secret));
    }

    #[tokio::test]
    async fn hidden_image_search_does_not_leak_ocr_snippets() {
        let database = test_database().await;
        insert_search_clip(
            &database,
            SearchFixture {
                id: "shot",
                clip_type: "image",
                content: "",
                preview: "Screenshot",
                ocr: Some("recovery code 8821 on the screenshot"),
                folder_id: None,
                pinned: false,
                created_at: "2026-05-02 09:00:00",
                source_app: None,
            },
        )
        .await;

        let visible = search_clips_in_database(
            "8821".into(),
            None,
            10,
            0,
            None,
            None,
            None,
            None,
            None,
            &database,
        )
        .await
        .unwrap();
        assert_eq!(visible.len(), 1);
        assert!(
            visible[0].ocr_match.is_some(),
            "a visible image search should still show the OCR snippet"
        );

        assert!(toggle_clip_hidden_in_pool(&database.pool, "shot")
            .await
            .expect("toggle should hide"));

        let found = search_clips_in_database(
            "8821".into(),
            None,
            10,
            0,
            None,
            None,
            None,
            None,
            None,
            &database,
        )
        .await
        .unwrap();
        assert_eq!(found.len(), 1, "a hidden image is still searchable");
        assert!(found[0].is_hidden);
        assert!(found[0].content.is_empty());
        assert!(
            found[0].ocr_match.is_none(),
            "the OCR snippet is the hidden text"
        );
        assert!(
            found[0].ocr_highlights.is_none(),
            "highlight boxes are made of the same OCR words"
        );
    }

    #[tokio::test]
    async fn delete_folder_unfiles_member_clips() {
        let database = test_database().await;
        let folder_id = sqlx::query("INSERT INTO folders (name) VALUES ('Receipts')")
            .execute(&database.pool)
            .await
            .unwrap()
            .last_insert_rowid();
        insert_search_clip(
            &database,
            SearchFixture {
                id: "filed",
                clip_type: "text",
                content: "invoice 42",
                preview: "invoice 42",
                ocr: None,
                folder_id: Some(folder_id),
                pinned: false,
                created_at: "2026-05-03 09:00:00",
                source_app: None,
            },
        )
        .await;

        delete_folder_in_pool(&database.pool, folder_id)
            .await
            .expect("a folder with clips should delete");

        let remaining_folders: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM folders")
            .fetch_one(&database.pool)
            .await
            .unwrap();
        assert_eq!(remaining_folders, 0);

        let remaining_folder_id: Option<i64> =
            sqlx::query_scalar("SELECT folder_id FROM clips WHERE uuid = 'filed'")
                .fetch_one(&database.pool)
                .await
                .unwrap();
        assert_eq!(remaining_folder_id, None, "members should be unfiled");
    }

    #[tokio::test]
    async fn remove_duplicates_deletes_soft_deleted_image_files() {
        let database = test_database().await;
        std::fs::create_dir_all(&database.image_dir).unwrap();

        insert_search_clip(
            &database,
            SearchFixture::text("keeper", "visible copy", "2026-05-04 09:00:00"),
        )
        .await;

        sqlx::query(
            r#"
            INSERT INTO clips
                (uuid, clip_type, content, text_preview, content_hash, is_deleted)
            VALUES ('gone', 'image', X'', 'Screenshot', 'hash-gone', 1)
            "#,
        )
        .execute(&database.pool)
        .await
        .unwrap();

        let gone_path = database.image_dir.join("gone.cubby");
        std::fs::write(&gone_path, b"full-resolution-bytes").unwrap();
        sqlx::query(
            r#"
            INSERT INTO clip_images (clip_uuid, full_content, file_path, file_size, storage_kind)
            VALUES ('gone', X'', ?, 21, 'file')
            "#,
        )
        .bind(gone_path.to_string_lossy().as_ref())
        .execute(&database.pool)
        .await
        .unwrap();

        let deleted = remove_duplicate_clips_in_database(&database)
            .await
            .expect("duplicate cleanup should succeed");
        assert_eq!(deleted, 1);

        let remaining: Vec<String> = sqlx::query_scalar("SELECT uuid FROM clips ORDER BY uuid")
            .fetch_all(&database.pool)
            .await
            .unwrap();
        assert_eq!(remaining, vec!["keeper"]);
        assert!(
            !gone_path.exists(),
            "the full-resolution file must not outlive the clip row"
        );
    }

    #[tokio::test]
    async fn source_app_counts_group_encrypted_app_names() {
        let database = test_database().await;
        seed_filter_clips(&database).await;
        database
            .search_index
            .ensure_ready(&database.pool, &database.crypto)
            .await
            .unwrap();

        // Each row's app name is encrypted under a fresh nonce, so identical
        // apps have different ciphertexts and SQL could not group them. The
        // counts still have to come out right, most used first.
        assert_eq!(
            database.search_index.source_app_counts(),
            vec![("code.exe".to_string(), 2), ("chrome.exe".to_string(), 1)]
        );
    }

    #[tokio::test]
    async fn get_clips_skips_an_unreadable_content_row() {
        let database = test_database().await;
        insert_search_clip(
            &database,
            SearchFixture::text("readable", "visible neighbor", "2026-07-01 09:00:00")
                .with_app("code.exe"),
        )
        .await;
        insert_search_clip(
            &database,
            SearchFixture::text("broken-content", "should vanish", "2026-07-01 10:00:00")
                .with_app("chrome.exe"),
        )
        .await;
        corrupt_clip_content(&database, "broken-content").await;

        let items =
            get_clips_in_database(None, 20, 0, Some(true), None, None, None, None, &database)
                .await
                .expect("one unreadable neighbor must not fail the listing");
        assert_eq!(
            items
                .iter()
                .map(|clip| clip.id.as_str())
                .collect::<Vec<_>>(),
            vec!["readable"]
        );

        // Single-clip reads still fail for the broken row itself.
        assert!(get_clip_details_in_database(&database, "broken-content")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn get_clips_skips_an_unreadable_preview_row() {
        let database = test_database().await;
        insert_search_clip(
            &database,
            SearchFixture::text("readable", "visible neighbor", "2026-07-02 09:00:00")
                .with_app("code.exe"),
        )
        .await;
        insert_search_clip(
            &database,
            SearchFixture::text("broken-preview", "should vanish", "2026-07-02 10:00:00")
                .with_app("chrome.exe"),
        )
        .await;
        corrupt_clip_preview(&database, "broken-preview").await;

        let items =
            get_clips_in_database(None, 20, 0, Some(true), None, None, None, None, &database)
                .await
                .expect("one unreadable preview must not fail the listing");
        assert_eq!(
            items
                .iter()
                .map(|clip| clip.id.as_str())
                .collect::<Vec<_>>(),
            vec!["readable"]
        );
    }

    #[tokio::test]
    async fn get_clips_page_survives_an_unreadable_neighbor() {
        let database = test_database().await;
        insert_search_clip(
            &database,
            SearchFixture::text("newer", "page newer", "2026-07-03 12:00:00"),
        )
        .await;
        insert_search_clip(
            &database,
            SearchFixture::text("broken-page", "page broken", "2026-07-03 11:00:00"),
        )
        .await;
        insert_search_clip(
            &database,
            SearchFixture::text("older", "page older", "2026-07-03 10:00:00"),
        )
        .await;
        corrupt_clip_content(&database, "broken-page").await;

        // A page of 20 used to fail entirely once the middle row refused to
        // decrypt. The readable neighbors on that page must still come back.
        let items =
            get_clips_in_database(None, 20, 0, Some(true), None, None, None, None, &database)
                .await
                .expect("a page must not become an IPC error for one bad row");
        assert_eq!(
            items
                .iter()
                .map(|clip| clip.id.as_str())
                .collect::<Vec<_>>(),
            vec!["newer", "older"]
        );
    }

    #[tokio::test]
    async fn search_clips_and_ensure_ready_skip_an_unreadable_clip() {
        let database = test_database().await;
        insert_search_clip(
            &database,
            SearchFixture::text("good-content", "alpha readable body", "2026-07-04 09:00:00")
                .with_app("code.exe"),
        )
        .await;
        insert_search_clip(
            &database,
            SearchFixture::text("good-preview", "bravo readable body", "2026-07-04 10:00:00")
                .with_app("code.exe"),
        )
        .await;
        insert_search_clip(
            &database,
            SearchFixture::text(
                "broken-content",
                "alpha should vanish",
                "2026-07-04 11:00:00",
            )
            .with_app("chrome.exe"),
        )
        .await;
        insert_search_clip(
            &database,
            SearchFixture::text(
                "broken-preview",
                "bravo should vanish",
                "2026-07-04 12:00:00",
            )
            .with_app("chrome.exe"),
        )
        .await;
        corrupt_clip_content(&database, "broken-content").await;
        corrupt_clip_preview(&database, "broken-preview").await;

        database
            .search_index
            .ensure_ready(&database.pool, &database.crypto)
            .await
            .expect("a corrupt payload must not leave the index unbuilt");

        assert!(database
            .search_index
            .matches("readable")
            .contains("good-content"));
        assert!(database
            .search_index
            .matches("readable")
            .contains("good-preview"));
        assert!(!database
            .search_index
            .matches("vanish")
            .contains("broken-content"));
        assert!(!database
            .search_index
            .matches("vanish")
            .contains("broken-preview"));
        assert_eq!(
            database.search_index.source_app_counts(),
            vec![("code.exe".to_string(), 2)]
        );

        let found = search_clips_in_database(
            "readable".into(),
            None,
            20,
            0,
            None,
            None,
            None,
            None,
            None,
            &database,
        )
        .await
        .expect("search must not fail the page for one unreadable clip");
        assert_eq!(
            found
                .iter()
                .map(|clip| clip.id.as_str())
                .collect::<Vec<_>>(),
            vec!["good-preview", "good-content"]
        );

        // Source-app listing goes through ensure_ready + ids_for_source_app.
        assert_eq!(
            listed_ids(&database, None, None, Some("code.exe")).await,
            vec!["good-preview", "good-content"]
        );
        assert!(listed_ids(&database, None, None, Some("chrome.exe"))
            .await
            .is_empty());
    }

    #[tokio::test]
    async fn get_clips_keeps_a_clip_whose_source_app_is_unreadable() {
        let database = test_database().await;
        insert_search_clip(
            &database,
            SearchFixture::text("broken-app", "readable body", "2026-07-05 09:00:00")
                .with_app("code.exe"),
        )
        .await;
        corrupt_clip_source_app(&database, "broken-app").await;

        let items =
            get_clips_in_database(None, 20, 0, Some(true), None, None, None, None, &database)
                .await
                .expect("a decorative field must not fail the listing");
        assert_eq!(
            items
                .iter()
                .map(|clip| clip.id.as_str())
                .collect::<Vec<_>>(),
            vec!["broken-app"],
            "content and preview decrypt, so the clip stays in the list"
        );
        assert_eq!(items[0].source_app, None, "the broken field is cleared");
    }

    /// The clients send the last displayed id as `after_id` and treat a short
    /// page as the end of history, so a page has to hold `limit` *readable*
    /// clips. A displayed-row offset alone is not enough once a neighbor was
    /// skipped (SBS-830 / SBS-993).
    #[tokio::test]
    async fn paging_hands_back_every_readable_clip_around_an_unreadable_one() {
        let database = test_database().await;
        let ids = seed_paging_clips(&database, 26).await;
        // Listing position 5, well inside the first page of 20.
        corrupt_clip_content(&database, &ids[5]).await;
        let readable: Vec<&str> = ids
            .iter()
            .filter(|id| *id != &ids[5])
            .map(String::as_str)
            .collect();

        let first = paged_ids(&database, 20, 0).await;
        assert_eq!(first.len(), 20, "a full page, not 19");
        assert_eq!(first, readable[..20]);

        let second = paged_after(&database, 20, first.last().expect("first page")).await;
        assert_eq!(second, readable[20..], "the tail is still reachable");

        let mut seen = first;
        seen.extend(second);
        assert_eq!(seen, readable, "no duplicate and no missing clip");
    }

    #[tokio::test]
    async fn paging_fills_pages_when_unreadable_clips_are_interleaved() {
        let database = test_database().await;
        let ids = seed_paging_clips(&database, 30).await;
        let broken: Vec<String> = [0_usize, 4, 9, 18, 23]
            .iter()
            .map(|position| ids[*position].clone())
            .collect();
        for id in &broken {
            corrupt_clip_content(&database, id).await;
        }
        let readable: Vec<&str> = ids
            .iter()
            .filter(|id| !broken.contains(id))
            .map(String::as_str)
            .collect();
        assert_eq!(readable.len(), 25);

        let first = paged_ids(&database, 20, 0).await;
        let second = paged_after(&database, 20, first.last().expect("first page")).await;
        assert_eq!(first.len(), 20);
        assert_eq!(second.len(), 5, "the short page is the real end of history");
        let mut seen = first;
        seen.extend(second);
        assert_eq!(seen, readable);
    }

    /// An image clip is indexed from its preview, so a corrupt thumbnail is a
    /// search hit that only fails when the page is decrypted.
    #[tokio::test]
    async fn search_paging_survives_a_hit_that_fails_to_decrypt() {
        let database = test_database().await;
        let mut ids = seed_paging_clips(&database, 25).await;
        insert_search_clip(
            &database,
            SearchFixture {
                id: "page-image",
                clip_type: "image",
                content: "pageable image bytes",
                preview: "pageable body",
                ocr: None,
                folder_id: None,
                pinned: false,
                created_at: "2026-07-01 12:19:30",
                source_app: None,
            },
        )
        .await;
        corrupt_clip_content(&database, "page-image").await;
        // Newest first: the image sits between minute 20 and minute 19.
        ids.insert(5, "page-image".to_string());
        let readable: Vec<&str> = ids
            .iter()
            .filter(|id| *id != "page-image")
            .map(String::as_str)
            .collect();

        database
            .search_index
            .ensure_ready(&database.pool, &database.crypto)
            .await
            .expect("the index should build");
        assert!(
            database
                .search_index
                .matches("pageable")
                .contains("page-image"),
            "the corrupt thumbnail is still a search candidate"
        );

        let first = searched_ids(&database, 20, 0).await;
        assert_eq!(first.len(), 20, "a full page of readable hits");
        assert_eq!(first, readable[..20]);
        let second = searched_after(&database, 20, first.last().expect("first page")).await;
        assert_eq!(second, readable[20..]);
    }

    /// SBS-993: a later page must bind the source offset on the first fetch
    /// instead of walking and decrypting `[0, offset)`.
    #[tokio::test]
    async fn later_page_does_not_rescan_the_readable_prefix() {
        let database = test_database().await;
        let ids = seed_paging_clips(&database, 60).await;
        let starts = std::sync::Mutex::new(Vec::<usize>::new());
        let pool = &database.pool;
        let page = super::collect_readable_clips(&database, 40, 10, |start, count| {
            starts.lock().expect("starts").push(start);
            async move {
                let rows = sqlx::query_as::<_, crate::models::Clip>(
                    r#"SELECT * FROM clips WHERE is_deleted = 0
                       ORDER BY is_pinned DESC, created_at DESC, uuid DESC
                       LIMIT ? OFFSET ?"#,
                )
                .bind(count as i64)
                .bind(start as i64)
                .fetch_all(pool)
                .await
                .map_err(|error| error.to_string())?;
                Ok(super::ClipBatch {
                    exhausted: rows.len() < count,
                    rows,
                })
            }
        })
        .await
        .expect("later page should load");
        assert_eq!(
            *starts.lock().expect("starts"),
            vec![40],
            "one fetch at the source offset, not a walk from 0"
        );
        assert_eq!(
            page.iter()
                .map(|clip| clip.uuid.as_str())
                .collect::<Vec<_>>(),
            ids[40..50].iter().map(String::as_str).collect::<Vec<_>>(),
        );
    }

    /// A gone cursor is unknown, not "start over": restarting would re-send
    /// the prefix the client already has.
    #[tokio::test]
    async fn gone_listing_cursor_falls_back_to_offset_not_the_start() {
        let database = test_database().await;
        let ids = seed_paging_clips(&database, 8).await;
        let page = super::get_clips_paged(
            None,
            3,
            5,
            None,
            None,
            None,
            None,
            None,
            Some("missing-cursor".into()),
            &database,
        )
        .await
        .expect("a gone cursor must not fail the listing");
        assert_eq!(
            page.iter().map(|clip| clip.id.as_str()).collect::<Vec<_>>(),
            ids[5..8].iter().map(String::as_str).collect::<Vec<_>>(),
            "fallback offset 5, not a restart at 0"
        );
    }

    #[test]
    fn source_start_keeps_a_missing_cursor_from_restarting() {
        let ids = ["a".into(), "b".into(), "c".into(), "d".into()];
        assert_eq!(super::source_start_for_ids(&ids, Some("b"), 0), 2);
        assert_eq!(super::source_start_for_ids(&ids, Some("gone"), 3), 3);
        assert_eq!(super::source_start_for_ids(&ids, Some("  "), 3), 3);
        assert_eq!(super::source_start_for_ids(&ids, None, 3), 3);
    }

    #[tokio::test]
    async fn source_app_filter_narrows_the_list_and_pages_correctly() {
        let database = test_database().await;
        seed_filter_clips(&database).await;

        assert_eq!(
            listed_ids(&database, None, None, Some("code.exe")).await,
            vec!["day-three", "day-one"]
        );
        // Matching is case-insensitive: the stored name is whatever Windows
        // reported at capture time.
        assert_eq!(
            listed_ids(&database, None, None, Some("CODE.EXE")).await,
            vec!["day-three", "day-one"]
        );
        assert!(listed_ids(&database, None, None, Some("nothing.exe"))
            .await
            .is_empty());

        // Paging runs over the filtered set, not the raw table: page two of a
        // one-per-page code.exe listing is the *second code.exe clip*, not
        // whatever row happens to sit second overall.
        let page_two = get_clips_in_database(
            None,
            1,
            1,
            Some(true),
            None,
            None,
            None,
            Some("code.exe".into()),
            &database,
        )
        .await
        .unwrap();
        assert_eq!(page_two.len(), 1);
        assert_eq!(page_two[0].id, "day-one");
    }

    /// Pins SBS-773: the Info request log that release builds persist must
    /// not contain the selected source-app filter value.
    #[test]
    fn get_clips_request_log_omits_raw_source_app_filter() {
        let marker = "UniqueBankingApp.exe";
        let line = get_clips_request_log(
            Some("12"),
            true,
            Some("text"),
            Some("2026-03-01 00:00:00"),
            Some("2026-03-02 00:00:00"),
            Some(marker),
        );
        assert!(
            !line.contains(marker),
            "release Info log must not contain the selected source app: {line}"
        );
        assert!(
            line.contains("source_app: set"),
            "a real selection should be categorical, got {line}"
        );
        assert!(
            line.contains("filter_id: Some(\"12\")"),
            "folder id is not source-app metadata and stays in the line: {line}"
        );
    }

    /// Pins SBS-773: not asked, blank, and set stay distinct so a missing
    /// filter cannot be read as "the user chose an empty app".
    #[test]
    fn source_app_filter_log_state_keeps_none_blank_and_set_apart() {
        assert_eq!(source_app_filter_log_state(None), "none");
        assert_eq!(source_app_filter_log_state(Some("")), "blank");
        assert_eq!(source_app_filter_log_state(Some("   ")), "blank");
        assert_eq!(source_app_filter_log_state(Some("code.exe")), "set");
        assert!(
            get_clips_request_log(None, false, None, None, None, None).contains("source_app: none")
        );
        assert!(
            get_clips_request_log(None, false, None, None, None, Some(""))
                .contains("source_app: blank")
        );
    }

    #[tokio::test]
    async fn date_range_filter_is_half_open_and_combines_with_source_app() {
        let database = test_database().await;
        seed_filter_clips(&database).await;

        // [from, to): the second day is included, the third is not.
        assert_eq!(
            listed_ids(
                &database,
                Some("2026-03-02 00:00:00"),
                Some("2026-03-03 00:00:00"),
                None
            )
            .await,
            vec!["day-two"]
        );
        assert_eq!(
            listed_ids(&database, Some("2026-03-02 00:00:00"), None, None).await,
            vec!["day-three", "day-two"]
        );
        assert_eq!(
            listed_ids(&database, None, Some("2026-03-02 00:00:00"), None).await,
            vec!["day-one"]
        );

        // Filters combine with AND: day-two is in range but is not code.exe.
        assert!(listed_ids(
            &database,
            Some("2026-03-02 00:00:00"),
            Some("2026-03-03 00:00:00"),
            Some("code.exe")
        )
        .await
        .is_empty());
        assert_eq!(
            listed_ids(
                &database,
                Some("2026-03-03 00:00:00"),
                None,
                Some("code.exe")
            )
            .await,
            vec!["day-three"]
        );
    }

    #[tokio::test]
    async fn search_combines_with_date_and_source_app_filters() {
        let database = test_database().await;
        seed_filter_clips(&database).await;

        let all = search_clips_in_database(
            "alpha".into(),
            None,
            50,
            0,
            None,
            None,
            None,
            None,
            None,
            &database,
        )
        .await
        .unwrap();
        assert_eq!(all.len(), 3);

        let narrowed = search_clips_in_database(
            "alpha".into(),
            None,
            50,
            0,
            None,
            None,
            Some("2026-03-02 00:00:00".into()),
            None,
            Some("code.exe".into()),
            &database,
        )
        .await
        .unwrap();
        assert_eq!(
            narrowed.into_iter().map(|clip| clip.id).collect::<Vec<_>>(),
            vec!["day-three"]
        );
    }

    #[tokio::test]
    async fn indexed_search_preserves_order_filters_pagination_and_encryption() {
        let database = test_database().await;
        let folder_id = sqlx::query("INSERT INTO folders (name) VALUES ('Receipts')")
            .execute(&database.pool)
            .await
            .unwrap()
            .last_insert_rowid();
        insert_search_clip(
            &database,
            SearchFixture {
                id: "text-result",
                clip_type: "text",
                content: "Alpha release confirmation",
                preview: "Alpha release confirmation",
                ocr: None,
                folder_id: Some(folder_id),
                pinned: false,
                created_at: "2026-01-01 00:00:00",
                source_app: Some("chrome.exe"),
            },
        )
        .await;
        insert_search_clip(
            &database,
            SearchFixture {
                id: "ocr-result",
                clip_type: "image",
                content: "",
                preview: "Screenshot",
                ocr: Some("Alpha receipt 8372"),
                folder_id: None,
                pinned: true,
                created_at: "2026-01-02 00:00:00",
                source_app: Some("SnippingTool.exe"),
            },
        )
        .await;
        insert_search_clip(
            &database,
            SearchFixture {
                id: "unrelated",
                clip_type: "text",
                content: "Beta notes",
                preview: "Beta notes",
                ocr: None,
                folder_id: None,
                pinned: false,
                created_at: "2026-01-03 00:00:00",
                source_app: Some("chrome.exe"),
            },
        )
        .await;

        let first = search_clips_in_database(
            "ALPHA".into(),
            None,
            1,
            0,
            None,
            None,
            None,
            None,
            None,
            &database,
        )
        .await
        .unwrap();
        assert_eq!(first[0].id, "ocr-result");
        assert!(first[0].ocr_match.is_some());
        assert!(first[0].has_ocr_text);
        assert_eq!(
            load_recognized_text(&database, "ocr-result").await.unwrap(),
            "Alpha receipt 8372"
        );

        let second = search_clips_in_database(
            "alpha".into(),
            None,
            1,
            1,
            None,
            None,
            None,
            None,
            None,
            &database,
        )
        .await
        .unwrap();
        assert_eq!(second[0].id, "text-result");

        let folder = search_clips_in_database(
            "alpha".into(),
            Some(folder_id.to_string()),
            10,
            0,
            None,
            None,
            None,
            None,
            None,
            &database,
        )
        .await
        .unwrap();
        assert_eq!(folder.len(), 1);
        assert_eq!(folder[0].id, "text-result");

        // Content tabs filter in the query itself so pagination stays correct.
        let images_only = search_clips_in_database(
            "alpha".into(),
            None,
            10,
            0,
            None,
            Some("images".into()),
            None,
            None,
            None,
            &database,
        )
        .await
        .unwrap();
        assert_eq!(images_only.len(), 1);
        assert_eq!(images_only[0].id, "ocr-result");

        let text_only = search_clips_in_database(
            "alpha".into(),
            None,
            10,
            0,
            None,
            Some("text".into()),
            None,
            None,
            None,
            &database,
        )
        .await
        .unwrap();
        assert_eq!(text_only.len(), 1);
        assert_eq!(text_only[0].id, "text-result");

        let persisted_search_tables: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE lower(name) LIKE '%search%' OR lower(sql) LIKE '%fts%'",
        )
        .fetch_one(&database.pool)
        .await
        .unwrap();
        assert_eq!(persisted_search_tables, 0);
    }

    #[test]
    fn ocr_match_centers_and_highlights_the_query() {
        let ocr_text = "The application could not start because the Windows clipboard history service is unavailable. Restart the computer and try again.";
        let matched = build_ocr_match(ocr_text, "windows clipboard history")
            .expect("OCR query should produce a visible match");

        assert_eq!(matched.matched, "Windows clipboard history");
        assert!(matched.before.starts_with('…'));
        assert!(matched.before.ends_with("because the "));
        assert!(matched.after.starts_with(" service is unavailable."));
        assert!(matched.after.ends_with('…'));
        assert!(
            format!("{}{}{}", matched.before, matched.matched, matched.after)
                .chars()
                .count()
                <= OCR_SNIPPET_CHAR_LIMIT
        );
    }

    #[test]
    fn ocr_match_is_case_insensitive_and_collapses_line_breaks() {
        let matched = build_ocr_match(
            "Receipt total\n\nCONFIRMATION NUMBER\nABCD-1234",
            "confirmation number",
        )
        .expect("case-insensitive OCR query should match");

        assert_eq!(matched.before, "Receipt total ");
        assert_eq!(matched.matched, "CONFIRMATION NUMBER");
        assert_eq!(matched.after, " ABCD-1234");
    }

    #[test]
    fn ocr_match_returns_none_for_unrelated_text() {
        assert_eq!(
            build_ocr_match("A recipe for tomato soup", "error code"),
            None
        );
        assert_eq!(build_ocr_match("Some OCR text", "   "), None);
    }

    #[tokio::test]
    async fn pin_toggle_round_trips_persisted_state() {
        let database = test_database().await;
        sqlx::query(
            r#"
            INSERT INTO clips (uuid, clip_type, content, text_preview, content_hash)
            VALUES ('clip-1', 'text', X'68656C6C6F', 'hello', 'hash-1')
            "#,
        )
        .execute(&database.pool)
        .await
        .expect("clip should be inserted");

        assert!(toggle_clip_pin_in_pool(&database.pool, "clip-1")
            .await
            .expect("first toggle should pin"));
        assert!(!toggle_clip_pin_in_pool(&database.pool, "clip-1")
            .await
            .expect("second toggle should unpin"));
        assert_eq!(
            toggle_clip_pin_in_pool(&database.pool, "missing").await,
            Err("Clipboard item not found".to_string())
        );
    }

    #[tokio::test]
    async fn storage_migration_encrypts_plaintext_history_and_is_idempotent() {
        let database = test_database().await;
        sqlx::query(
            r#"
            INSERT INTO clips
                (uuid, clip_type, content, text_preview, content_hash, source_app, metadata)
            VALUES
                ('legacy-text', 'text', ?, 'private preview', 'legacy-sha', 'Editor.exe', '{"kind":"text"}')
            "#,
        )
        .bind(b"private clipboard payload".as_slice())
        .execute(&database.pool)
        .await
        .expect("legacy clip should be inserted");

        assert_eq!(migrate_encrypted_storage(&database).await.unwrap(), 1);
        assert_eq!(migrate_encrypted_storage(&database).await.unwrap(), 0);

        let mut stored: Clip = sqlx::query_as("SELECT * FROM clips WHERE uuid = 'legacy-text'")
            .fetch_one(&database.pool)
            .await
            .expect("migrated clip should load");
        assert!(database.crypto.is_encrypted(&stored.content));
        assert!(database.crypto.is_encrypted_text(&stored.text_preview));
        assert!(database
            .crypto
            .is_encrypted_text(stored.source_app.as_deref().unwrap()));
        assert_ne!(stored.content_hash, "legacy-sha");

        super::decrypt_clip_fields(&database, &mut stored).unwrap();
        assert_eq!(stored.content, b"private clipboard payload");
        assert_eq!(stored.text_preview, "private preview");
        assert_eq!(stored.source_app.as_deref(), Some("Editor.exe"));
        assert_eq!(stored.metadata.as_deref(), Some("{\"kind\":\"text\"}"));
    }

    #[tokio::test]
    async fn storage_migration_replaces_plaintext_images_with_encrypted_files_and_previews() {
        let database = test_database().await;
        std::fs::create_dir_all(&database.image_dir).unwrap();
        let old_path = database.image_dir.join("legacy-image.png");
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(4, 3)
            .write_to(&mut png, image::ImageOutputFormat::Png)
            .unwrap();
        let png = png.into_inner();
        std::fs::write(&old_path, &png).unwrap();

        sqlx::query(
            r#"
            INSERT INTO clips (uuid, clip_type, content, text_preview, content_hash, metadata)
            VALUES ('legacy-image', 'image', x'', '[Image]', 'legacy-image-sha', '{"width":4,"height":3}')
            "#,
        )
        .execute(&database.pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO clip_images (clip_uuid, full_content, file_path, file_size, storage_kind)
            VALUES ('legacy-image', x'', ?, ?, 'file')
            "#,
        )
        .bind(old_path.to_string_lossy().to_string())
        .bind(png.len() as i64)
        .execute(&database.pool)
        .await
        .unwrap();

        assert_eq!(migrate_encrypted_storage(&database).await.unwrap(), 1);
        let mut stored: Clip = sqlx::query_as("SELECT * FROM clips WHERE uuid = 'legacy-image'")
            .fetch_one(&database.pool)
            .await
            .unwrap();
        let (new_path, storage_kind): (String, String) = sqlx::query_as(
            "SELECT file_path, storage_kind FROM clip_images WHERE clip_uuid = 'legacy-image'",
        )
        .fetch_one(&database.pool)
        .await
        .unwrap();

        assert!(database.crypto.is_encrypted(&stored.content));
        assert_eq!(storage_kind, "encrypted_file");
        assert!(new_path.ends_with("legacy-image.cubby"));
        assert!(!old_path.exists());
        let encrypted_file = std::fs::read(&new_path).unwrap();
        assert!(database.crypto.is_encrypted(&encrypted_file));
        assert_eq!(database.crypto.decrypt(&encrypted_file).unwrap(), png);
        super::decrypt_clip_fields(&database, &mut stored).unwrap();
        assert!(!stored.content.is_empty());
        image::load_from_memory(&stored.content).expect("decrypted preview should be a PNG");

        std::fs::remove_dir_all(&database.image_dir).unwrap();
    }

    #[tokio::test]
    async fn storage_migration_never_deletes_images_outside_its_profile() {
        let database = test_database().await;
        std::fs::create_dir_all(&database.image_dir).unwrap();
        let external_dir = std::env::temp_dir().join(format!(
            "cubby-external-image-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&external_dir).unwrap();
        let external_path = external_dir.join("legacy-image.png");
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgba8(4, 3)
            .write_to(&mut png, image::ImageOutputFormat::Png)
            .unwrap();
        let png = png.into_inner();
        std::fs::write(&external_path, &png).unwrap();

        sqlx::query(
            r#"
            INSERT INTO clips (uuid, clip_type, content, text_preview, content_hash)
            VALUES ('external-image', 'image', x'', '[Image]', 'legacy-image-sha')
            "#,
        )
        .execute(&database.pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO clip_images (clip_uuid, full_content, file_path, file_size, storage_kind)
            VALUES ('external-image', x'', ?, ?, 'file')
            "#,
        )
        .bind(external_path.to_string_lossy().to_string())
        .bind(png.len() as i64)
        .execute(&database.pool)
        .await
        .unwrap();

        assert_eq!(migrate_encrypted_storage(&database).await.unwrap(), 1);
        assert!(external_path.exists());
        let migrated_path: String = sqlx::query_scalar(
            "SELECT file_path FROM clip_images WHERE clip_uuid = 'external-image'",
        )
        .fetch_one(&database.pool)
        .await
        .unwrap();
        assert!(std::path::Path::new(&migrated_path).starts_with(&database.image_dir));
        assert!(database
            .crypto
            .is_encrypted(&std::fs::read(migrated_path).unwrap()));

        std::fs::remove_dir_all(&database.image_dir).unwrap();
        std::fs::remove_dir_all(external_dir).unwrap();
    }

    #[test]
    fn retention_file_cleanup_stays_inside_the_managed_image_directory() {
        let root =
            std::env::temp_dir().join(format!("cubby-cleanup-test-{}", uuid::Uuid::new_v4()));
        let image_dir = root.join("images");
        let external_dir = root.join("external");
        std::fs::create_dir_all(&image_dir).unwrap();
        std::fs::create_dir_all(&external_dir).unwrap();
        let managed = image_dir.join("managed.cubby");
        let external = external_dir.join("keep.cubby");
        std::fs::write(&managed, b"managed").unwrap();
        std::fs::write(&external, b"external").unwrap();

        remove_clip_image_files(
            &image_dir,
            vec![
                managed.to_string_lossy().to_string(),
                external.to_string_lossy().to_string(),
            ],
        );

        assert!(!managed.exists());
        assert!(external.exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn restore_model_preserves_rich_formats_and_plain_text_override() {
        let clip = Clip {
            id: 1,
            uuid: "rich".to_string(),
            clip_type: "text".to_string(),
            content: b"Hello".to_vec(),
            text_preview: "Hello".to_string(),
            content_hash: "hash".to_string(),
            folder_id: None,
            is_deleted: false,
            is_pinned: false,
            is_thumbnail: false,
            source_app: None,
            source_icon: None,
            metadata: None,
            ocr_text: None,
            ocr_words: None,
            full_image_expired: false,
            notes: None,
            is_hidden: false,
            created_at: chrono::Utc::now(),
            last_accessed: chrono::Utc::now(),
        };
        let formats = vec![
            ("html".to_string(), b"<b>Hello</b>".to_vec()),
            ("rtf".to_string(), br"{\rtf1\b Hello}".to_vec()),
        ];

        let rich = clipboard_contents_for_restore(&clip, None, &formats, false).unwrap();
        assert!(matches!(&rich[0], ClipboardContent::Text(text) if text == "Hello"));
        // HTML must go out as full CF_HTML — clipboard-rs's multi-content set()
        // writes the string raw, and Office rejects a headerless payload.
        assert!(matches!(
            &rich[1],
            ClipboardContent::Html(html)
                if html.starts_with("Version:0.9\r\nStartHTML:")
                    && html.contains("<b>Hello</b>")
        ));
        assert!(matches!(&rich[2], ClipboardContent::Rtf(rtf) if rtf.contains("Hello")));

        let plain = clipboard_contents_for_restore(&clip, None, &formats, true).unwrap();
        assert_eq!(plain.len(), 1);
        assert!(matches!(&plain[0], ClipboardContent::Text(text) if text == "Hello"));
        assert_eq!(
            restore_hash_material(&clip, None, &formats, true),
            b"text\0Hello"
        );
        assert_ne!(
            restore_hash_material(&clip, None, &formats, false),
            restore_hash_material(&clip, None, &formats, true)
        );
        let mut image_clip = clip.clone();
        image_clip.clip_type = "image".to_string();
        let files = vec![("files".to_string(), br#"["C:\\one.png"]"#.to_vec())];
        let other_files = vec![("files".to_string(), br#"["D:\\other-shot.png"]"#.to_vec())];
        assert_eq!(
            restore_hash_material(&image_clip, Some(b"full image"), &files, false),
            restore_hash_material(&image_clip, Some(b"full image"), &other_files, false),
        );

        let mut png = Vec::new();
        image::DynamicImage::new_rgba8(1, 1)
            .write_to(
                &mut std::io::Cursor::new(&mut png),
                image::ImageOutputFormat::Png,
            )
            .unwrap();
        let image_contents =
            clipboard_contents_for_restore(&image_clip, Some(&png), &files, false).unwrap();
        assert_eq!(image_contents.len(), 1);
        assert!(matches!(&image_contents[0], ClipboardContent::Image(_)));
    }

    #[tokio::test]
    async fn format_model_migration_rekeys_existing_encrypted_clips_once() {
        let database = test_database().await;
        sqlx::query(
            r#"
            INSERT INTO clips (uuid, clip_type, content, text_preview, content_hash)
            VALUES ('rich', 'text', ?, ?, 'old-hash')
            "#,
        )
        .bind(database.crypto.encrypt(b"Hello").unwrap())
        .bind(database.crypto.encrypt_text("Hello").unwrap())
        .execute(&database.pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO clips (uuid, clip_type, content, text_preview, content_hash)
            VALUES ('image', 'image', ?, ?, 'old-image-hash')
            "#,
        )
        .bind(database.crypto.encrypt(b"png bytes").unwrap())
        .bind(database.crypto.encrypt_text("Image").unwrap())
        .execute(&database.pool)
        .await
        .unwrap();
        crate::clipboard::replace_clip_formats(
            &database.pool,
            &database.crypto,
            "rich",
            &[CapturedFormat {
                name: "html",
                content: b"<b>Hello</b>".to_vec(),
            }],
        )
        .await
        .unwrap();
        crate::clipboard::replace_clip_formats(
            &database.pool,
            &database.crypto,
            "image",
            &[CapturedFormat {
                name: "files",
                content: br#"["C:\\shot.png"]"#.to_vec(),
            }],
        )
        .await
        .unwrap();
        let encrypted_format: Vec<u8> =
            sqlx::query_scalar("SELECT content FROM clip_formats WHERE clip_uuid = 'rich'")
                .fetch_one(&database.pool)
                .await
                .unwrap();
        assert!(database.crypto.is_encrypted(&encrypted_format));

        assert_eq!(migrate_clip_format_model(&database).await.unwrap(), 2);
        assert_eq!(migrate_clip_format_model(&database).await.unwrap(), 0);
        let hash: String = sqlx::query_scalar("SELECT content_hash FROM clips WHERE uuid = 'rich'")
            .fetch_one(&database.pool)
            .await
            .unwrap();
        let expected = database
            .crypto
            .keyed_hash(b"text\0Hello\0html\0<b>Hello</b>");
        assert_eq!(hash, expected);
        let image_hash: String =
            sqlx::query_scalar("SELECT content_hash FROM clips WHERE uuid = 'image'")
                .fetch_one(&database.pool)
                .await
                .unwrap();
        assert_eq!(image_hash, database.crypto.keyed_hash(b"image\0png bytes"));
    }

    #[tokio::test]
    async fn bulk_clear_preserves_only_active_pinned_clips() {
        let database = test_database().await;
        for (uuid, pinned, deleted) in [
            ("pinned", 1, 0),
            ("ordinary", 0, 0),
            ("deleted-pinned", 1, 1),
        ] {
            sqlx::query(
                r#"
                INSERT INTO clips
                    (uuid, clip_type, content, text_preview, content_hash, is_pinned, is_deleted)
                VALUES (?, 'image', X'', ?, ?, ?, ?)
                "#,
            )
            .bind(uuid)
            .bind(uuid)
            .bind(format!("hash-{uuid}"))
            .bind(pinned)
            .bind(deleted)
            .execute(&database.pool)
            .await
            .expect("clip should be inserted");

            sqlx::query(
                r#"
                INSERT INTO clip_images (clip_uuid, full_content, file_path)
                VALUES (?, X'', ?)
                "#,
            )
            .bind(uuid)
            .bind(format!("{uuid}.png"))
            .execute(&database.pool)
            .await
            .expect("image metadata should be inserted");

            sqlx::query(
                "INSERT INTO clip_formats (clip_uuid, format, content) VALUES (?, 'html', x'31')",
            )
            .bind(uuid)
            .execute(&database.pool)
            .await
            .expect("format metadata should be inserted");
        }
        let (deleted, image_paths) = clear_clips_in_pool(&database.pool, true)
            .await
            .expect("clear should succeed");
        assert_eq!(deleted, 2);
        assert_eq!(image_paths.len(), 2);

        let remaining_clips: Vec<String> =
            sqlx::query_scalar("SELECT uuid FROM clips ORDER BY uuid")
                .fetch_all(&database.pool)
                .await
                .expect("remaining clips should load");
        let remaining_images: Vec<String> =
            sqlx::query_scalar("SELECT clip_uuid FROM clip_images ORDER BY clip_uuid")
                .fetch_all(&database.pool)
                .await
                .expect("remaining image metadata should load");
        let remaining_formats: Vec<String> =
            sqlx::query_scalar("SELECT clip_uuid FROM clip_formats ORDER BY clip_uuid")
                .fetch_all(&database.pool)
                .await
                .expect("remaining format metadata should load");
        assert_eq!(remaining_clips, vec!["pinned"]);
        assert_eq!(remaining_images, vec!["pinned"]);
        assert_eq!(remaining_formats, vec!["pinned"]);

        let (deleted, _) = clear_clips_in_pool(&database.pool, false)
            .await
            .expect("full clear should succeed");
        assert_eq!(deleted, 1);
        let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM clips")
            .fetch_one(&database.pool)
            .await
            .expect("clip count should load");
        assert_eq!(remaining, 0);
    }

    #[tokio::test]
    async fn retention_preserves_pins_and_removes_expired_and_overflow_items() {
        let database = test_database().await;
        for (uuid, pinned, age_days) in [
            ("pinned", 1, 90),
            ("expired", 0, 60),
            ("recent-1", 0, 3),
            ("recent-2", 0, 2),
            ("recent-3", 0, 1),
        ] {
            sqlx::query(
                r#"
                INSERT INTO clips
                    (uuid, clip_type, content, text_preview, content_hash, is_pinned, created_at)
                VALUES (?, 'text', x'31', ?, ?, ?, datetime('now', '-' || ? || ' days'))
                "#,
            )
            .bind(uuid)
            .bind(uuid)
            .bind(format!("hash-{uuid}"))
            .bind(pinned)
            .bind(age_days)
            .execute(&database.pool)
            .await
            .expect("fixture should insert");
        }

        let (deleted, _) = enforce_retention_in_pool(&database.pool, 2, 30)
            .await
            .expect("retention should succeed");
        assert_eq!(deleted, 2);

        let remaining: Vec<String> = sqlx::query_scalar("SELECT uuid FROM clips ORDER BY uuid")
            .fetch_all(&database.pool)
            .await
            .expect("remaining clips should load");
        assert_eq!(remaining, vec!["pinned", "recent-2", "recent-3"]);
    }

    /// Insert an aged image clip plus its `clip_images` blob row for the
    /// SOU-244 retention tests.
    async fn insert_aged_image(
        database: &Database,
        uuid: &str,
        age_days: i64,
        is_pinned: i64,
        ocr_status: &str,
        ocr_text: Option<&str>,
    ) {
        sqlx::query(
            r#"
            INSERT INTO clips
                (uuid, clip_type, content, text_preview, content_hash, is_pinned,
                 ocr_status, ocr_text, created_at, last_accessed)
            VALUES (?, 'image', x'89504e47', '[Image]', ?, ?, ?, ?,
                    datetime('now', '-' || ? || ' days'),
                    datetime('now', '-' || ? || ' days'))
            "#,
        )
        .bind(uuid)
        .bind(format!("hash-{uuid}"))
        .bind(is_pinned)
        .bind(ocr_status)
        .bind(ocr_text)
        .bind(age_days)
        .bind(age_days)
        .execute(&database.pool)
        .await
        .expect("image fixture should insert");

        sqlx::query(
            r#"
            INSERT INTO clip_images
                (clip_uuid, full_content, file_path, file_size, storage_kind, mime_type)
            VALUES (?, x'', ?, 1024, 'file', 'image/png')
            "#,
        )
        .bind(uuid)
        .bind(format!("C:/images/{uuid}.cubby"))
        .execute(&database.pool)
        .await
        .expect("clip_images fixture should insert");
    }

    async fn clip_uuids(database: &Database) -> Vec<String> {
        sqlx::query_scalar("SELECT uuid FROM clips ORDER BY uuid")
            .fetch_all(&database.pool)
            .await
            .expect("clips should load")
    }

    #[tokio::test]
    async fn retention_preserves_ocr_images_and_drops_only_their_full_blob() {
        let database = test_database().await;
        // Aged past the 30-day window, with recognized text -> preserved.
        insert_aged_image(&database, "ocr-old", 60, 0, "completed", Some("CUB1:text")).await;
        // Aged out but no recognized text -> fully deleted (nothing to keep).
        insert_aged_image(&database, "ocr-old-empty", 60, 0, "completed", None).await;
        // Aged out but OCR never finished -> fully deleted.
        insert_aged_image(&database, "pending-old", 60, 0, "pending", None).await;
        // Recent image with text -> untouched (not past the window).
        insert_aged_image(
            &database,
            "ocr-recent",
            1,
            0,
            "completed",
            Some("CUB1:text"),
        )
        .await;
        // Pinned image, aged, with text -> untouched (pins are always kept whole).
        insert_aged_image(
            &database,
            "ocr-pinned",
            90,
            1,
            "completed",
            Some("CUB1:text"),
        )
        .await;

        // max_items = 0 isolates the age window; auto_delete_days = 30.
        let (deleted, paths) = enforce_retention_in_pool(&database.pool, 0, 30)
            .await
            .expect("retention should succeed");

        // Only the two textless aged images are hard-deleted.
        assert_eq!(deleted, 2);
        // Every dropped full-image blob (preserved + deleted) is returned for
        // disk cleanup, including the preserved clip's file.
        let mut paths = paths;
        paths.sort();
        assert_eq!(
            paths,
            vec![
                "C:/images/ocr-old-empty.cubby",
                "C:/images/ocr-old.cubby",
                "C:/images/pending-old.cubby",
            ]
        );

        // Rows kept: the preserved image plus the two untouched ones.
        assert_eq!(
            clip_uuids(&database).await,
            vec!["ocr-old", "ocr-pinned", "ocr-recent"]
        );

        // The preserved image keeps its text but is flagged and stripped of its blob.
        let (expired, ocr_text): (bool, Option<String>) =
            sqlx::query_as("SELECT full_image_expired, ocr_text FROM clips WHERE uuid = 'ocr-old'")
                .fetch_one(&database.pool)
                .await
                .expect("preserved clip should load");
        assert!(expired, "aged OCR image should be flagged expired");
        assert_eq!(ocr_text.as_deref(), Some("CUB1:text"));
        let blob_rows: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM clip_images WHERE clip_uuid = 'ocr-old'")
                .fetch_one(&database.pool)
                .await
                .expect("blob count should load");
        assert_eq!(blob_rows, 0, "preserved image's full blob should be gone");

        // Untouched images keep both their row flag and their blob.
        let untouched_blob: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM clip_images WHERE clip_uuid = 'ocr-recent'")
                .fetch_one(&database.pool)
                .await
                .expect("blob count should load");
        assert_eq!(untouched_blob, 1);

        // Re-running retention is stable: the preserved clip is never re-swept.
        let (deleted_again, paths_again) = enforce_retention_in_pool(&database.pool, 0, 30)
            .await
            .expect("second retention pass should succeed");
        assert_eq!(deleted_again, 0);
        assert!(paths_again.is_empty());
        assert_eq!(
            clip_uuids(&database).await,
            vec!["ocr-old", "ocr-pinned", "ocr-recent"]
        );

        // Explicitly deleting a preserved clip still wipes it entirely.
        sqlx::query("UPDATE clips SET is_deleted = 1 WHERE uuid = 'ocr-old'")
            .execute(&database.pool)
            .await
            .expect("soft delete should apply");
        let (deleted_explicit, _) = enforce_retention_in_pool(&database.pool, 0, 30)
            .await
            .expect("third retention pass should succeed");
        assert_eq!(deleted_explicit, 1);
        assert_eq!(
            clip_uuids(&database).await,
            vec!["ocr-pinned", "ocr-recent"]
        );
    }

    #[test]
    fn directory_size_sums_nested_files_and_ignores_missing() {
        let root = std::env::temp_dir().join(format!("cubby-size-{}", uuid::Uuid::new_v4()));
        let images = root.join("images");
        std::fs::create_dir_all(&images).expect("temp dirs should create");
        std::fs::write(root.join("cubby.db"), vec![0u8; 500]).expect("db file should write");
        std::fs::write(images.join("a.cubby"), vec![0u8; 1000]).expect("image should write");
        std::fs::write(images.join("b.cubby"), vec![0u8; 24]).expect("image should write");

        // Recurses into subdirectories and sums every file.
        assert_eq!(directory_size_bytes(&root, None), 1524);

        // A path that does not exist measures as zero rather than erroring.
        let missing = root.join("does-not-exist");
        assert_eq!(directory_size_bytes(&missing, None), 0);

        std::fs::remove_dir_all(&root).ok();
    }

    /// Portable mode writes logs to `<data_dir>/logs`, inside the directory the
    /// storage readout measures. The log file is not history, so "Storage used"
    /// and the reclaim before/after must not count it.
    #[test]
    fn directory_size_skips_the_portable_log_directory() {
        let root = std::env::temp_dir().join(format!("cubby-logsize-{}", uuid::Uuid::new_v4()));
        let images = root.join("images");
        let logs = crate::portable_log_dir(Some(root.clone())).expect("portable root has logs");
        std::fs::create_dir_all(&images).expect("temp dirs should create");
        std::fs::create_dir_all(&logs).expect("log dir should create");
        std::fs::write(root.join("cubby.db"), vec![0u8; 500]).expect("db file should write");
        std::fs::write(images.join("a.cubby"), vec![0u8; 24]).expect("image should write");
        std::fs::write(logs.join("Cubby.log"), vec![0u8; 9000]).expect("log file should write");

        // History bytes only: the 9000-byte log is left out.
        assert_eq!(directory_size_bytes(&root, Some(&logs)), 524);

        // Without the exclusion the same tree measures the log file too, which is
        // the inflated number users used to see.
        assert_eq!(directory_size_bytes(&root, None), 9524);

        // A folder merely *named* logs elsewhere in the tree is still history.
        let nested = images.join("logs");
        std::fs::create_dir_all(&nested).expect("nested dir should create");
        std::fs::write(nested.join("b.cubby"), vec![0u8; 6]).expect("image should write");
        assert_eq!(directory_size_bytes(&root, Some(&logs)), 530);

        std::fs::remove_dir_all(&root).ok();
    }

    /// Which log folder, if any, the storage readout leaves out. Only a
    /// portable run puts logs inside the measured directory; an installed run
    /// keeps them under `%LOCALAPPDATA%`, so a `logs` folder found there is
    /// somebody's data and must be counted.
    #[test]
    fn only_a_portable_run_excludes_a_log_folder_from_storage_used() {
        let history = std::env::temp_dir().join(format!("cubby-excl-{}", uuid::Uuid::new_v4()));

        // Portable: the history data directory *is* the portable root.
        assert_eq!(
            excluded_log_dir(&history, Some(history.clone())),
            Some(history.join("logs")),
            "a portable run must leave its own log directory out"
        );

        // Installed: no portable root at all, so nothing is excluded and a
        // user's `logs` folder still counts toward Storage used.
        assert_eq!(excluded_log_dir(&history, None), None);

        // A portable install whose root is somewhere else entirely must not
        // make this directory's `logs` folder disappear from the measurement.
        assert_eq!(
            excluded_log_dir(&history, Some(history.join("elsewhere"))),
            None
        );
    }

    /// The wiring, not just the walker: what `Storage used` and the reclaim
    /// before/after report for the layout the test binary actually runs in,
    /// which is the installed one (no `portable.txt` beside the test exe).
    #[tokio::test]
    async fn history_disk_bytes_counts_an_installed_layout_whole() {
        let root = std::env::temp_dir().join(format!("cubby-history-{}", uuid::Uuid::new_v4()));
        let images = root.join("images");
        let logs = root.join("logs");
        std::fs::create_dir_all(&images).expect("temp dirs should create");
        std::fs::create_dir_all(&logs).expect("log dir should create");
        std::fs::write(root.join("cubby.db"), vec![0u8; 500]).expect("db file should write");
        std::fs::write(images.join("a.cubby"), vec![0u8; 24]).expect("image should write");

        let mut database = test_database().await;
        database.image_dir = images;

        let before = history_disk_bytes(&database)
            .await
            .expect("measurement should succeed");
        assert_eq!(before, 524);

        // Installed logs live under %LOCALAPPDATA%, so a `logs` folder here is
        // user data. Skipping it would under-report by that whole subtree.
        std::fs::write(logs.join("notes.txt"), vec![0u8; 9000]).expect("file should write");
        let after = history_disk_bytes(&database)
            .await
            .expect("measurement should succeed");
        assert_eq!(after, 9524, "an installed run must not skip a logs folder");

        std::fs::remove_dir_all(&root).ok();
    }

    /// Two lines of two words each, in reading order, in a 100x50 image.
    fn two_line_words(with_recorded_lines: bool) -> Vec<crate::ocr::OcrWordBox> {
        let make = |text: &str, x: f32, y: f32, line: u32| crate::ocr::OcrWordBox {
            text: text.to_string(),
            x,
            y,
            width: 20.0,
            height: 10.0,
            line: with_recorded_lines.then_some(line),
        };
        vec![
            make("hello", 0.0, 0.0, 0),
            make("there", 25.0, 0.0, 0),
            make("second", 0.0, 25.0, 1),
            make("line", 25.0, 25.0, 1),
        ]
    }

    fn layout_json(words: Vec<crate::ocr::OcrWordBox>) -> String {
        serde_json::to_string(&crate::ocr::OcrLayout {
            image_width: 100,
            image_height: 50,
            words,
        })
        .expect("layout should serialize")
    }

    #[tokio::test]
    async fn clip_details_return_full_text_and_the_selectable_word_layout() {
        let database = test_database().await;
        insert_search_clip(
            &database,
            SearchFixture {
                id: "full-text",
                clip_type: "text",
                content: "the whole body, not the row preview",
                preview: "the whole body...",
                ocr: None,
                source_app: None,
                folder_id: None,
                pinned: false,
                created_at: "2026-03-01 00:00:00",
            },
        )
        .await;

        // The pane shows the whole clip, not the truncated row preview.
        let details = get_clip_details_in_database(&database, "full-text")
            .await
            .expect("text details should load");
        assert_eq!(details.content, "the whole body, not the row preview");
        assert!(details.ocr_text.is_none());
        assert!(details.ocr_layout.is_none());
        assert!(!details.image_expired);

        let layout = crate::ocr::OcrLayout {
            image_width: 100,
            image_height: 50,
            words: vec![crate::ocr::OcrWordBox {
                text: "invoice".to_string(),
                x: 10.0,
                y: 5.0,
                width: 40.0,
                height: 10.0,
                line: Some(0),
            }],
        };
        sqlx::query(
            r#"
            INSERT INTO clips (uuid, clip_type, content, text_preview, content_hash, ocr_text, ocr_words, full_image_expired)
            VALUES ('shot', 'image', ?, ?, 'hash-shot', ?, ?, 1)
            "#,
        )
        .bind(database.crypto.encrypt(&[1, 2, 3]).unwrap())
        .bind(database.crypto.encrypt_text("Screenshot").unwrap())
        .bind(database.crypto.encrypt_text("invoice").unwrap())
        .bind(
            database
                .crypto
                .encrypt_text(&serde_json::to_string(&layout).unwrap())
                .unwrap(),
        )
        .execute(&database.pool)
        .await
        .unwrap();

        let details = get_clip_details_in_database(&database, "shot")
            .await
            .expect("image details should load");
        // Retention dropped the full blob, so the surviving thumbnail comes back
        // flagged rather than being passed off as the original.
        assert!(details.image_expired);
        assert_eq!(details.ocr_text.as_deref(), Some("invoice"));
        let words = details.ocr_layout.expect("layout should be returned").words;
        assert_eq!(words.len(), 1);
        assert_eq!(words[0].text, "invoice");
        // Fractions of the image, so the overlay lines up at any zoom.
        assert_eq!(words[0].x, 0.1);
        assert_eq!(words[0].width, 0.4);
    }

    #[tokio::test]
    async fn clip_details_report_a_missing_clip_rather_than_returning_empty() {
        let database = test_database().await;
        assert!(get_clip_details_in_database(&database, "nope")
            .await
            .is_err());
    }

    #[test]
    fn ocr_text_layout_normalizes_boxes_and_keeps_recorded_lines() {
        let layout = ocr_text_layout(&layout_json(two_line_words(true)))
            .expect("layout should build from recorded lines");

        assert_eq!(layout.aspect, 2.0);
        assert_eq!(
            layout.words.iter().map(|w| w.line).collect::<Vec<_>>(),
            vec![0, 0, 1, 1]
        );
        // Boxes arrive as fractions of the image, not raw pixels.
        assert_eq!(layout.words[1].x, 0.25);
        assert_eq!(layout.words[2].y, 0.5);
        assert_eq!(layout.words[0].width, 0.2);
        assert_eq!(layout.words[0].height, 0.2);
    }

    #[test]
    fn ocr_text_layout_infers_lines_for_layouts_stored_without_them() {
        // Legacy layouts have no line indices. Without inference every word
        // would land on line 0 and a two-line selection would copy as one line.
        let layout = ocr_text_layout(&layout_json(two_line_words(false)))
            .expect("layout should build from inferred lines");

        assert_eq!(
            layout.words.iter().map(|w| w.line).collect::<Vec<_>>(),
            vec![0, 0, 1, 1]
        );
    }

    #[test]
    fn ocr_text_layout_densifies_gaps_in_recorded_line_indices() {
        // The engine skips empty words, so its indices can jump. Consecutive
        // stored lines must still come out one apart.
        let words = vec![
            crate::ocr::OcrWordBox {
                text: "a".to_string(),
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
                line: Some(0),
            },
            crate::ocr::OcrWordBox {
                text: "b".to_string(),
                x: 0.0,
                y: 20.0,
                width: 10.0,
                height: 10.0,
                line: Some(7),
            },
        ];
        let layout = ocr_text_layout(&layout_json(words)).expect("layout should build");
        assert_eq!(
            layout.words.iter().map(|w| w.line).collect::<Vec<_>>(),
            vec![0, 1]
        );
    }

    #[test]
    fn ocr_text_layout_rejects_unusable_input() {
        assert!(ocr_text_layout("not json").is_none());
        assert!(ocr_text_layout(&layout_json(Vec::new())).is_none());
        // Zero dimensions would make every fraction a division by zero.
        let zero = serde_json::to_string(&crate::ocr::OcrLayout {
            image_width: 0,
            image_height: 0,
            words: two_line_words(true),
        })
        .expect("layout should serialize");
        assert!(ocr_text_layout(&zero).is_none());
    }

    #[test]
    fn ocr_highlights_selects_matching_words_as_image_fractions() {
        use crate::ocr::{OcrLayout, OcrWordBox};
        let layout = OcrLayout {
            image_width: 100,
            image_height: 50,
            words: vec![
                OcrWordBox {
                    text: "Error".to_string(),
                    x: 10.0,
                    y: 5.0,
                    width: 40.0,
                    height: 10.0,
                    line: Some(0),
                },
                OcrWordBox {
                    text: "Denied".to_string(),
                    x: 60.0,
                    y: 5.0,
                    width: 30.0,
                    height: 10.0,
                    line: Some(0),
                },
                OcrWordBox {
                    text: "Ok".to_string(),
                    x: 0.0,
                    y: 30.0,
                    width: 10.0,
                    height: 8.0,
                    line: Some(1),
                },
            ],
        };
        let json = serde_json::to_string(&layout).expect("layout should serialize");

        // Single-token, case-insensitive: one matched box, coordinates as fractions.
        let hits = build_ocr_highlights(&json, "error").expect("should match a word");
        assert_eq!(hits.aspect, 2.0);
        assert_eq!(hits.boxes.len(), 1);
        let rect = &hits.boxes[0];
        assert!((rect.x - 0.10).abs() < 1e-6);
        assert!((rect.y - 0.10).abs() < 1e-6);
        assert!((rect.width - 0.40).abs() < 1e-6);
        assert!((rect.height - 0.20).abs() < 1e-6);

        // Every word matching any query token is highlighted.
        let multi = build_ocr_highlights(&json, "error denied").expect("should match two words");
        assert_eq!(multi.boxes.len(), 2);

        // No matching word, and too-short tokens, both yield nothing.
        assert!(build_ocr_highlights(&json, "zzz").is_none());
        assert!(build_ocr_highlights(&json, "a").is_none());
    }
}
