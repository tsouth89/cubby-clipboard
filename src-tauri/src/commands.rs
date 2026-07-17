use crate::database::Database;
use crate::models::{Clip, ClipboardItem, Folder, FolderItem};
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
    }
}

fn decrypt_clip_fields(db: &Database, clip: &mut Clip) -> Result<(), String> {
    clip.content = db.crypto.decrypt(&clip.content)?;
    clip.text_preview = db.crypto.decrypt_text(&clip.text_preview)?;
    db.crypto.decrypt_optional_text(&mut clip.source_app)?;
    db.crypto.decrypt_optional_text(&mut clip.source_icon)?;
    db.crypto.decrypt_optional_text(&mut clip.metadata)?;
    // OCR text is auxiliary; never let a bad value block loading the clip.
    let _ = db.crypto.decrypt_optional_text(&mut clip.ocr_text);
    Ok(())
}

fn clip_to_detail_item(clip: &Clip, full_image_content: Option<&[u8]>) -> ClipboardItem {
    let content_str = if clip.clip_type == "image" {
        BASE64.encode(full_image_content.unwrap_or(&clip.content))
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
    }
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

#[tauri::command]
pub async fn get_clip(
    id: String,
    db: tauri::State<'_, Arc<Database>>,
) -> Result<ClipboardItem, String> {
    let pool = &db.pool;

    let clip: Option<Clip> = sqlx::query_as(r#"SELECT * FROM clips WHERE uuid = ?"#)
        .bind(&id)
        .fetch_optional(pool)
        .await
        .map_err(|e| e.to_string())?;

    match clip {
        Some(mut clip) => {
            if clip.clip_type == "image" {
                let full = load_full_image_content(&db, &mut clip).await?;
                decrypt_clip_fields(&db, &mut clip)?;
                Ok(clip_to_detail_item(&clip, Some(&full)))
            } else {
                decrypt_clip_fields(&db, &mut clip)?;
                Ok(clip_to_detail_item(&clip, None))
            }
        }
        None => Err("Clip not found".to_string()),
    }
}

// TODO(xueshi) get_clip is same as get_clip_detail???
#[tauri::command]
pub async fn get_clip_detail(
    id: String,
    db: tauri::State<'_, Arc<Database>>,
) -> Result<ClipboardItem, String> {
    get_clip(id, db).await
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
    let pool = &db.pool;
    let started = Instant::now();

    let normalized_query = query.to_lowercase();
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
    let batch_size = requested_limit.saturating_mul(4).clamp(100, 500) as i64;
    let mut database_offset = 0_i64;
    let mut matched = 0_usize;
    let mut sql_ms = 0_u128;
    let mut clips = Vec::with_capacity(requested_limit);

    'batches: loop {
        let sql_started = Instant::now();
        let mut batch: Vec<Clip> = if let Some(folder_id) = folder_id {
            sqlx::query_as(
                r#"
                    SELECT * FROM clips
                    WHERE is_deleted = 0 AND folder_id = ?
                    ORDER BY is_pinned DESC, created_at DESC
                    LIMIT ? OFFSET ?
                "#,
            )
            .bind(folder_id)
            .bind(batch_size)
            .bind(database_offset)
            .fetch_all(pool)
            .await
            .map_err(|e| e.to_string())?
        } else {
            sqlx::query_as(
                r#"
                    SELECT * FROM clips
                    WHERE is_deleted = 0
                    ORDER BY is_pinned DESC, created_at DESC
                    LIMIT ? OFFSET ?
                "#,
            )
            .bind(batch_size)
            .bind(database_offset)
            .fetch_all(pool)
            .await
            .map_err(|e| e.to_string())?
        };
        sql_ms += sql_started.elapsed().as_millis();
        let batch_len = batch.len();
        if batch_len == 0 {
            break;
        }

        for mut clip in batch.drain(..) {
            decrypt_clip_fields(&db, &mut clip)?;
            let is_match = String::from_utf8_lossy(&clip.content)
                .to_lowercase()
                .contains(&normalized_query)
                || clip.text_preview.to_lowercase().contains(&normalized_query)
                || clip
                    .ocr_text
                    .as_deref()
                    .is_some_and(|text| text.to_lowercase().contains(&normalized_query));
            if !is_match {
                continue;
            }
            if matched < requested_offset {
                matched += 1;
                continue;
            }
            clips.push(clip);
            if clips.len() == requested_limit {
                break 'batches;
            }
        }

        database_offset += batch_len as i64;
        if batch_len < batch_size as usize {
            break;
        }
    }

    let image_rows = clips
        .iter()
        .filter(|clip| clip.clip_type == "image")
        .count();
    let raw_bytes: usize = clips.iter().map(|clip| clip.content.len()).sum();
    let map_started = Instant::now();
    let items: Vec<ClipboardItem> = clips.iter().map(clip_to_list_item).collect();
    let map_ms = map_started.elapsed().as_millis();
    let total_ms = started.elapsed().as_millis();
    log::info!(
        "[perf][search_clips] sql_ms={} map_ms={} total_ms={} rows={} images={} raw_bytes={} filter_id={:?} offset={} limit={}",
        sql_ms,
        map_ms,
        total_ms,
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
pub fn hide_window(window: tauri::WebviewWindow) -> Result<(), String> {
    window.hide().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn ping() -> Result<String, String> {
    Ok("pong".to_string())
}

#[tauri::command]
pub fn test_log() -> Result<String, String> {
    log::trace!("[TEST] Trace level log");
    log::debug!("[TEST] Debug level log");
    log::info!("[TEST] Info level log");
    log::warn!("[TEST] Warn level log");
    log::error!("[TEST] Error level log");
    Ok("Logs emitted - check console".to_string())
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

#[tauri::command]
pub async fn clear_clipboard_history(db: tauri::State<'_, Arc<Database>>) -> Result<(), String> {
    let pool = &db.pool;

    sqlx::query(r#"DELETE FROM clips WHERE is_deleted = 1"#)
        .execute(pool)
        .await
        .map_err(|e| e.to_string())?;
    cleanup_orphan_clip_image_files(pool, &db.image_dir).await?;
    Ok(())
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

pub(crate) async fn enforce_retention_in_pool(
    pool: &SqlitePool,
    max_items: i64,
    auto_delete_days: i64,
) -> Result<(u64, Vec<String>), String> {
    let candidate_query = r#"
        SELECT uuid FROM clips
        WHERE is_pinned = 0 AND (
            is_deleted = 1
            OR (? > 0 AND created_at < datetime('now', '-' || ? || ' days'))
            OR (? > 0 AND uuid IN (
                SELECT uuid FROM clips
                WHERE is_deleted = 0 AND is_pinned = 0
                ORDER BY created_at DESC
                LIMIT -1 OFFSET ?
            ))
        )
    "#;

    let candidates: Vec<String> = sqlx::query_scalar(candidate_query)
        .bind(auto_delete_days)
        .bind(auto_delete_days)
        .bind(max_items)
        .bind(max_items.max(0))
        .fetch_all(pool)
        .await
        .map_err(|error| error.to_string())?;

    if candidates.is_empty() {
        return Ok((0, Vec::new()));
    }

    let placeholders = std::iter::repeat_n("?", candidates.len())
        .collect::<Vec<_>>()
        .join(",");
    let select_paths =
        format!("SELECT file_path FROM clip_images WHERE clip_uuid IN ({placeholders})");
    let delete_images = format!("DELETE FROM clip_images WHERE clip_uuid IN ({placeholders})");
    let delete_clips = format!("DELETE FROM clips WHERE uuid IN ({placeholders})");

    let mut transaction = pool.begin().await.map_err(|error| error.to_string())?;
    let mut path_query = sqlx::query_scalar::<_, Option<String>>(&select_paths);
    for id in &candidates {
        path_query = path_query.bind(id);
    }
    let image_paths = path_query
        .fetch_all(&mut *transaction)
        .await
        .map_err(|error| error.to_string())?;

    let mut image_delete = sqlx::query(&delete_images);
    for id in &candidates {
        image_delete = image_delete.bind(id);
    }
    image_delete
        .execute(&mut *transaction)
        .await
        .map_err(|error| error.to_string())?;

    let mut clip_delete = sqlx::query(&delete_clips);
    for id in &candidates {
        clip_delete = clip_delete.bind(id);
    }
    let deleted = clip_delete
        .execute(&mut *transaction)
        .await
        .map_err(|error| error.to_string())?
        .rows_affected();

    transaction
        .commit()
        .await
        .map_err(|error| error.to_string())?;

    Ok((deleted, image_paths.into_iter().flatten().collect()))
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
    Ok(deleted)
}

#[tauri::command]
pub async fn clear_all_clips(db: tauri::State<'_, Arc<Database>>) -> Result<(), String> {
    let (_, image_paths) = clear_clips_in_pool(&db.pool, false).await?;
    remove_clip_image_files(&db.image_dir, image_paths);
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

    Ok(result.rows_affected() as i64)
}

#[tauri::command]
pub async fn register_global_shortcut(
    hotkey: String,
    window: tauri::WebviewWindow,
) -> Result<(), String> {
    crate::shortcuts::register_standard_shortcut(window.app_handle(), &hotkey)
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
pub fn show_window(window: tauri::WebviewWindow) -> Result<(), String> {
    crate::position_window_near_cursor(&window);
    Ok(())
}

#[tauri::command]
pub async fn pick_file(app: AppHandle) -> Result<String, String> {
    use tauri_plugin_dialog::DialogExt;

    let file_path = app
        .dialog()
        .file()
        .add_filter("Executables", &["exe", "app"])
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
pub fn get_layout_config() -> serde_json::Value {
    serde_json::json!({
        "window_height": crate::constants::WINDOW_HEIGHT,
    })
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
    crate::ditto_import::import_from_ditto(&db, &db_path, dry_run).await
}

#[cfg(test)]
mod tests {
    use super::{
        clear_clips_in_pool, clipboard_contents_for_restore, enforce_retention_in_pool,
        migrate_clip_format_model, migrate_encrypted_storage, remove_clip_image_files,
        restore_hash_material, toggle_clip_pin_in_pool, ClipboardContent,
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
        };
        database.migrate().await.expect("migration should succeed");
        database
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
}
