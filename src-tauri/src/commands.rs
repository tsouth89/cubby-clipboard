use crate::database::Database;
use crate::models::{Clip, ClipboardItem, Folder, FolderItem, OcrHighlights, OcrMatch, OcrRect};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use clipboard_rs::common::RustImage;
use clipboard_rs::{Clipboard, ClipboardContent, ClipboardContext, RustImageData};
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use tauri::{AppHandle, Emitter, Manager};

fn clip_to_list_item(clip: &Clip) -> ClipboardItem {
    let content_str = if clip.clip_type == "image" {
        BASE64.encode(&clip.content)
    } else {
        String::from_utf8_lossy(&clip.content).to_string()
    };

    ClipboardItem {
        id: clip.uuid.clone(),
        clip_type: clip.clip_type.clone(),
        content: content_str,
        preview: clip.text_preview.clone(),
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
        ocr_match: None,
        ocr_highlights: None,
        image_expired: clip.full_image_expired,
    }
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

fn clip_to_search_item(clip: &Clip, query: &str) -> ClipboardItem {
    let mut item = clip_to_list_item(clip);
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
    clip.content = db.crypto.decrypt(&clip.content)?;
    clip.text_preview = db.crypto.decrypt_text(&clip.text_preview)?;
    db.crypto.decrypt_optional_text(&mut clip.source_app)?;
    db.crypto.decrypt_optional_text(&mut clip.source_icon)?;
    db.crypto.decrypt_optional_text(&mut clip.metadata)?;
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
    Ok(())
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

fn is_managed_image_path(image_dir: &std::path::Path, file_path: &str) -> bool {
    let Ok(managed_dir) = image_dir.canonicalize() else {
        return false;
    };
    let Some(parent) = std::path::Path::new(file_path).parent() else {
        return false;
    };
    parent
        .canonicalize()
        .map(|candidate| candidate == managed_dir)
        .unwrap_or(false)
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
            if old_path != *new_path && is_managed_image_path(&db.image_dir, &old_path) {
                crate::clipboard::remove_full_image_file(&old_path);
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
        let formats = load_clip_formats(db, &clip.uuid).await?;
        let full_image = if clip.clip_type == "image" {
            Some(load_full_image_content(db, clip).await?)
        } else {
            None
        };
        let mut hash_material = Vec::new();
        hash_material.extend_from_slice(clip.clip_type.as_bytes());
        hash_material.push(0);
        hash_material.extend_from_slice(full_image.as_deref().unwrap_or(&clip.content));
        for (format, content) in formats {
            hash_material.push(0);
            hash_material.extend_from_slice(format.as_bytes());
            hash_material.push(0);
            hash_material.extend_from_slice(&content);
        }
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

async fn load_full_image_content(db: &Database, clip: &mut Clip) -> Result<Vec<u8>, String> {
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
    let mut contents = if let Some(image) = full_image {
        vec![ClipboardContent::Image(
            RustImageData::from_bytes(image).map_err(|e| e.to_string())?,
        )]
    } else {
        vec![ClipboardContent::Text(plain_content)]
    };
    if !plain_text {
        for (format, content) in formats {
            match format.as_str() {
                "html" => contents.push(ClipboardContent::Html(
                    String::from_utf8(content.clone())
                        .map_err(|_| "stored HTML is not UTF-8".to_string())?,
                )),
                "rtf" => contents.push(ClipboardContent::Rtf(
                    String::from_utf8(content.clone())
                        .map_err(|_| "stored RTF is not UTF-8".to_string())?,
                )),
                "files" => contents.push(ClipboardContent::Files(
                    serde_json::from_slice(content)
                        .map_err(|_| "stored file list is invalid".to_string())?,
                )),
                _ => {}
            }
        }
    }
    Ok(contents)
}

#[tauri::command]
pub async fn get_clips(
    filter_id: Option<String>,
    limit: i64,
    offset: i64,
    preview_only: Option<bool>,
    db: tauri::State<'_, Arc<Database>>,
) -> Result<Vec<ClipboardItem>, String> {
    let pool = &db.pool;
    let preview_only = preview_only.unwrap_or(false);
    let started = Instant::now();

    log::info!(
        "get_clips called with filter_id: {:?}, preview_only: {}",
        filter_id,
        preview_only
    );

    let sql_started = Instant::now();
    let mut clips: Vec<Clip> = match filter_id.as_deref() {
        Some(id) => {
            let folder_id_num = id.parse::<i64>().ok();
            if let Some(numeric_id) = folder_id_num {
                log::info!("Querying for folder_id: {}", numeric_id);
                sqlx::query_as(
                    r#"
                    SELECT * FROM clips WHERE is_deleted = 0 AND folder_id = ?
                    ORDER BY is_pinned DESC, created_at DESC LIMIT ? OFFSET ?
                "#,
                )
                .bind(numeric_id)
                .bind(limit)
                .bind(offset)
                .fetch_all(pool)
                .await
                .map_err(|e| e.to_string())?
            } else {
                log::info!("Unknown folder_id, returning empty");
                Vec::new()
            }
        }
        None => {
            log::info!("Querying for items, offset: {}, limit: {}", offset, limit);
            sqlx::query_as(
                r#"
                SELECT * FROM clips WHERE is_deleted = 0
                ORDER BY is_pinned DESC, created_at DESC LIMIT ? OFFSET ?
            "#,
            )
            .bind(limit)
            .bind(offset)
            .fetch_all(pool)
            .await
            .map_err(|e| e.to_string())?
        }
    };
    let sql_ms = sql_started.elapsed().as_millis();

    log::info!("DB: Found {} clips", clips.len());
    for clip in &mut clips {
        decrypt_clip_fields(&db, clip)?;
    }

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
            let item = clip_to_list_item(clip);
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
        offset,
        limit
    );

    Ok(items)
}

fn restore_hash_material(
    clip: &Clip,
    full_image: Option<&[u8]>,
    formats: &[(String, Vec<u8>)],
    plain_text: bool,
) -> Vec<u8> {
    let mut material = Vec::new();
    if plain_text {
        material.extend_from_slice(b"text");
        material.push(0);
        material.extend_from_slice(&clip.content);
        return material;
    }

    material.extend_from_slice(clip.clip_type.as_bytes());
    material.push(0);
    material.extend_from_slice(full_image.unwrap_or(&clip.content));
    for (format, content) in formats {
        material.push(0);
        material.extend_from_slice(format.as_bytes());
        material.push(0);
        material.extend_from_slice(content);
    }
    material
}

async fn restore_clip(
    id: &str,
    plain_text: bool,
    should_paste: bool,
    window: &tauri::WebviewWindow,
    db: &Database,
) -> Result<(), String> {
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

            let formats = load_clip_formats(db, &clip.uuid).await?;
            let full_image = if clip.clip_type == "image" {
                Some(load_full_image_content(db, &mut clip).await?)
            } else {
                None
            };
            let hash_material =
                restore_hash_material(&clip, full_image.as_deref(), &formats, plain_text);
            let content_hash = crate::clipboard::calculate_hash(&hash_material);
            let uuid = clip.uuid.clone();

            let clipboard_contents =
                clipboard_contents_for_restore(&clip, full_image.as_deref(), &formats, plain_text)?;

            crate::clipboard::set_ignore_hash(content_hash);
            let final_res = ClipboardContext::new()
                .and_then(|context| context.set(clipboard_contents))
                .map_err(|error| format!("Failed to restore clipboard formats: {error}"));

            // Manually perform the LRU bump (update created_at)
            let _ =
                sqlx::query(r#"UPDATE clips SET created_at = CURRENT_TIMESTAMP WHERE uuid = ?"#)
                    .bind(&uuid)
                    .execute(pool)
                    .await;

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
                            crate::restore_previous_foreground_window();
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
                    crate::animate_window_hide(window, None);
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
    crate::clipboard::set_ignore_hash(crate::clipboard::calculate_hash(&hash_material));
    ClipboardContext::new()
        .and_then(|context| context.set(vec![ClipboardContent::Text(text.clone())]))
        .map_err(|error| format!("Failed to copy recognized text: {error}"))?;

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
                crate::restore_previous_foreground_window();
                if !crate::paste_engine::should_auto_paste_with_mode(strategy, &remote_paste_mode) {
                    log::info!("PASTE: Recognized text is ready; waiting for physical Ctrl+V");
                    return;
                }
                std::thread::sleep(crate::paste_engine::paste_settle_delay(strategy));
                crate::paste_engine::send_paste_input(strategy);
            })),
        );
    } else {
        crate::animate_window_hide(window, None);
    }
    Ok(())
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
    let pool = &db.pool;

    if hard_delete {
        let mut transaction = pool.begin().await.map_err(|e| e.to_string())?;
        let file_path: Option<String> =
            sqlx::query_scalar(r#"SELECT file_path FROM clip_images WHERE clip_uuid = ?"#)
                .bind(&id)
                .fetch_optional(&mut *transaction)
                .await
                .map_err(|e| e.to_string())?;
        sqlx::query(r#"DELETE FROM clips WHERE uuid = ?"#)
            .bind(&id)
            .execute(&mut *transaction)
            .await
            .map_err(|e| e.to_string())?;
        transaction.commit().await.map_err(|e| e.to_string())?;

        remove_clip_image_files(&db.image_dir, file_path.into_iter().collect());
    } else {
        sqlx::query(r#"UPDATE clips SET is_deleted = 1 WHERE uuid = ?"#)
            .bind(&id)
            .execute(pool)
            .await
            .map_err(|e| e.to_string())?;
    }
    db.search_index.remove(&id);
    Ok(())
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
    let pool = &db.pool;

    let folder_id: i64 = id.parse().map_err(|_| "Invalid folder ID")?;
    sqlx::query(r#"DELETE FROM folders WHERE id = ?"#)
        .bind(folder_id)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;

    let _ = window.emit("clipboard-change", ());
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
pub async fn search_clips(
    query: String,
    filter_id: Option<String>,
    limit: i64,
    offset: i64,
    db: tauri::State<'_, Arc<Database>>,
) -> Result<Vec<ClipboardItem>, String> {
    search_clips_in_database(query, filter_id, limit, offset, db.inner()).await
}

async fn search_clips_in_database(
    query: String,
    filter_id: Option<String>,
    limit: i64,
    offset: i64,
    db: &Database,
) -> Result<Vec<ClipboardItem>, String> {
    let pool = &db.pool;
    let started = Instant::now();
    let requested_offset = offset.max(0) as usize;
    let requested_limit = limit.max(0) as usize;
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
    let candidates = db.search_index.matches(&query);
    let index_ms = index_started.elapsed().as_millis();
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    // Ordering, pinning, folder filtering, and pagination remain authoritative
    // in SQLite. Only UUIDs are scanned here; encrypted payloads are fetched and
    // decrypted for the final result page.
    let sql_started = Instant::now();
    let ordered_ids: Vec<String> = if let Some(folder_id) = folder_id {
        sqlx::query_scalar(
            r#"
            SELECT uuid FROM clips
            WHERE is_deleted = 0 AND folder_id = ?
            ORDER BY is_pinned DESC, created_at DESC
            "#,
        )
        .bind(folder_id)
        .fetch_all(pool)
        .await
        .map_err(|error| error.to_string())?
    } else {
        sqlx::query_scalar(
            r#"
            SELECT uuid FROM clips
            WHERE is_deleted = 0
            ORDER BY is_pinned DESC, created_at DESC
            "#,
        )
        .fetch_all(pool)
        .await
        .map_err(|error| error.to_string())?
    };
    let selected_ids = ordered_ids
        .into_iter()
        .filter(|id| candidates.contains(id))
        .skip(requested_offset)
        .take(requested_limit)
        .collect::<Vec<_>>();
    if selected_ids.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders = std::iter::repeat_n("?", selected_ids.len())
        .collect::<Vec<_>>()
        .join(",");
    let selected_sql = format!(
        "SELECT * FROM clips WHERE is_deleted = 0 AND uuid IN ({placeholders}) \
         ORDER BY is_pinned DESC, created_at DESC"
    );
    let mut selected_query = sqlx::query_as::<_, Clip>(&selected_sql);
    for id in &selected_ids {
        selected_query = selected_query.bind(id);
    }
    let mut clips = selected_query
        .fetch_all(pool)
        .await
        .map_err(|error| error.to_string())?;
    let sql_ms = sql_started.elapsed().as_millis();
    for clip in &mut clips {
        decrypt_clip_fields(db, clip)?;
    }

    let image_rows = clips
        .iter()
        .filter(|clip| clip.clip_type == "image")
        .count();
    let raw_bytes: usize = clips.iter().map(|clip| clip.content.len()).sum();
    let map_started = Instant::now();
    let items: Vec<ClipboardItem> = clips
        .iter()
        .map(|clip| clip_to_search_item(clip, &query))
        .collect();
    let map_ms = map_started.elapsed().as_millis();
    let total_ms = started.elapsed().as_millis();
    log::info!(
        "[perf][search_clips] index_ms={} sql_ms={} map_ms={} total_ms={} candidates={} rows={} images={} raw_bytes={} filter_id={:?} offset={} limit={}",
        index_ms,
        sql_ms,
        map_ms,
        total_ms,
        candidates.len(),
        clips.len(),
        image_rows,
        raw_bytes,
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
/// (`image_dir` is `<data_dir>/images`). Tauri writes logs to a separate LogDir,
/// so everything here is history state.
fn history_data_dir(db: &Database) -> std::path::PathBuf {
    db.image_dir
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| db.image_dir.clone())
}

/// Recursively sum the size of every file under `path`. Missing or unreadable
/// entries are skipped rather than failing the whole measurement. Runs on a
/// blocking thread since it stat()s potentially thousands of image files.
fn directory_size_bytes(path: &std::path::Path) -> i64 {
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
                stack.push(entry.path());
            } else {
                total = total.saturating_add(metadata.len() as i64);
            }
        }
    }
    total
}

async fn history_disk_bytes(db: &Database) -> Result<i64, String> {
    let dir = history_data_dir(db);
    tokio::task::spawn_blocking(move || directory_size_bytes(&dir))
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
    // this set so they're processed at most once.
    let preserve_query = r#"
        SELECT uuid FROM clips
        WHERE is_pinned = 0
          AND is_deleted = 0
          AND clip_type = 'image'
          AND full_image_expired = 0
          AND ocr_status = 'completed'
          AND ocr_text IS NOT NULL
          AND (
              (? > 0 AND created_at < datetime('now', '-' || ? || ' days'))
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
                (? > 0 AND created_at < datetime('now', '-' || ? || ' days'))
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

pub(crate) fn remove_clip_image_files(image_dir: &std::path::Path, image_paths: Vec<String>) {
    for path in image_paths {
        if !path.is_empty() && is_managed_image_path(image_dir, &path) {
            crate::clipboard::remove_full_image_file(&path);
        } else if !path.is_empty() {
            log::warn!("Skipped deleting an unmanaged clipboard image path");
        }
    }
}

#[tauri::command]
pub async fn clear_unpinned_clips(db: tauri::State<'_, Arc<Database>>) -> Result<u64, String> {
    let (deleted, image_paths) = clear_clips_in_pool(&db.pool, true).await?;
    remove_clip_image_files(&db.image_dir, image_paths);
    if deleted > 0 {
        db.search_index.invalidate();
    }
    Ok(deleted)
}

#[tauri::command]
pub async fn clear_all_clips(db: tauri::State<'_, Arc<Database>>) -> Result<(), String> {
    let (_, image_paths) = clear_clips_in_pool(&db.pool, false).await?;
    remove_clip_image_files(&db.image_dir, image_paths);
    db.search_index.invalidate();
    Ok(())
}

#[tauri::command]
pub async fn remove_duplicate_clips(db: tauri::State<'_, Arc<Database>>) -> Result<i64, String> {
    let pool = &db.pool;

    let result = sqlx::query(
        r#"
        DELETE FROM clips
        WHERE id NOT IN (
            SELECT MIN(id)
            FROM clips
            GROUP BY content_hash
        )
    "#,
    )
    .execute(pool)
    .await
    .map_err(|e| e.to_string())?;

    cleanup_orphan_clip_image_files(pool, &db.image_dir).await?;

    if result.rows_affected() > 0 {
        db.search_index.invalidate();
    }

    Ok(result.rows_affected() as i64)
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
        Some(path) => Ok(path.to_string()),
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
    let result = crate::ditto_import::import_from_ditto(&db, &db_path, dry_run).await?;
    if !dry_run && result.imported > 0 {
        db.search_index.invalidate();
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::{
        build_ocr_highlights, build_ocr_match, clear_clips_in_pool, clipboard_contents_for_restore,
        directory_size_bytes, enforce_retention_in_pool, load_recognized_text,
        migrate_clip_format_model, migrate_encrypted_storage, remove_clip_image_files,
        restore_hash_material, search_clips_in_database, toggle_clip_pin_in_pool, ClipboardContent,
        OCR_SNIPPET_CHAR_LIMIT,
    };
    use crate::clipboard::CapturedFormat;
    use crate::database::Database;
    use crate::models::Clip;
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
    }

    async fn insert_search_clip(database: &Database, fixture: SearchFixture<'_>) {
        let encrypted_content = database.crypto.encrypt(fixture.content.as_bytes()).unwrap();
        let encrypted_preview = database.crypto.encrypt_text(fixture.preview).unwrap();
        let encrypted_ocr = fixture
            .ocr
            .map(|text| database.crypto.encrypt_text(text).unwrap());
        sqlx::query(
            r#"
            INSERT INTO clips (
                uuid, clip_type, content, text_preview, content_hash,
                folder_id, is_pinned, created_at, ocr_text
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
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
        .execute(&database.pool)
        .await
        .unwrap();
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
            },
        )
        .await;

        let first = search_clips_in_database("ALPHA".into(), None, 1, 0, &database)
            .await
            .unwrap();
        assert_eq!(first[0].id, "ocr-result");
        assert!(first[0].ocr_match.is_some());
        assert!(first[0].has_ocr_text);
        assert_eq!(
            load_recognized_text(&database, "ocr-result").await.unwrap(),
            "Alpha receipt 8372"
        );

        let second = search_clips_in_database("alpha".into(), None, 1, 1, &database)
            .await
            .unwrap();
        assert_eq!(second[0].id, "text-result");

        let folder = search_clips_in_database(
            "alpha".into(),
            Some(folder_id.to_string()),
            10,
            0,
            &database,
        )
        .await
        .unwrap();
        assert_eq!(folder.len(), 1);
        assert_eq!(folder[0].id, "text-result");

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
            created_at: chrono::Utc::now(),
            last_accessed: chrono::Utc::now(),
        };
        let formats = vec![
            ("html".to_string(), b"<b>Hello</b>".to_vec()),
            ("rtf".to_string(), br"{\rtf1\b Hello}".to_vec()),
        ];

        let rich = clipboard_contents_for_restore(&clip, None, &formats, false).unwrap();
        assert!(matches!(&rich[0], ClipboardContent::Text(text) if text == "Hello"));
        assert!(matches!(&rich[1], ClipboardContent::Html(html) if html == "<b>Hello</b>"));
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

        let files = vec![(
            "files".to_string(),
            serde_json::to_vec(&vec![r"C:\one.txt", r"C:\two.txt"]).unwrap(),
        )];
        let file_contents = clipboard_contents_for_restore(&clip, None, &files, false).unwrap();
        assert!(matches!(&file_contents[1], ClipboardContent::Files(paths) if paths.len() == 2));
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
        let encrypted_format: Vec<u8> =
            sqlx::query_scalar("SELECT content FROM clip_formats WHERE clip_uuid = 'rich'")
                .fetch_one(&database.pool)
                .await
                .unwrap();
        assert!(database.crypto.is_encrypted(&encrypted_format));

        assert_eq!(migrate_clip_format_model(&database).await.unwrap(), 1);
        assert_eq!(migrate_clip_format_model(&database).await.unwrap(), 0);
        let hash: String = sqlx::query_scalar("SELECT content_hash FROM clips WHERE uuid = 'rich'")
            .fetch_one(&database.pool)
            .await
            .unwrap();
        let expected = database
            .crypto
            .keyed_hash(b"text\0Hello\0html\0<b>Hello</b>");
        assert_eq!(hash, expected);
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
                 ocr_status, ocr_text, created_at)
            VALUES (?, 'image', x'89504e47', '[Image]', ?, ?, ?, ?,
                    datetime('now', '-' || ? || ' days'))
            "#,
        )
        .bind(uuid)
        .bind(format!("hash-{uuid}"))
        .bind(is_pinned)
        .bind(ocr_status)
        .bind(ocr_text)
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
        assert_eq!(directory_size_bytes(&root), 1524);

        // A path that does not exist measures as zero rather than erroring.
        let missing = root.join("does-not-exist");
        assert_eq!(directory_size_bytes(&missing), 0);

        std::fs::remove_dir_all(&root).ok();
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
                },
                OcrWordBox {
                    text: "Denied".to_string(),
                    x: 60.0,
                    y: 5.0,
                    width: 30.0,
                    height: 10.0,
                },
                OcrWordBox {
                    text: "Ok".to_string(),
                    x: 0.0,
                    y: 30.0,
                    width: 10.0,
                    height: 8.0,
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
