use tauri::{AppHandle, Emitter};
// Import functions directly from the crate root
use crate::database::Database;
#[cfg(target_os = "windows")]
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use clipboard_rs::common::RustImage;
use clipboard_rs::{Clipboard, ClipboardContext};
#[cfg(target_os = "windows")]
use clipboard_win::Monitor;
use once_cell::sync::Lazy;
use sha2::{Digest, Sha256};
#[cfg(target_os = "windows")]
use std::ffi::OsStr;
#[cfg(target_os = "windows")]
use std::os::windows::ffi::OsStrExt;
use std::sync::Arc;
use uuid::Uuid;
#[cfg(target_os = "windows")]
use windows::Win32::Foundation::MAX_PATH;
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    GetObjectW, ReleaseDC, SelectObject, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HBITMAP,
};
#[cfg(target_os = "windows")]
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};
#[cfg(target_os = "windows")]
use windows::Win32::System::DataExchange::GetClipboardOwner;
#[cfg(target_os = "windows")]
use windows::Win32::System::ProcessStatus::{GetModuleBaseNameW, GetModuleFileNameExW};
#[cfg(target_os = "windows")]
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ};
#[cfg(target_os = "windows")]
#[cfg(target_os = "windows")]
use windows::Win32::UI::Shell::{
    SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_USEFILEATTRIBUTES,
};
#[cfg(target_os = "windows")]
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, DrawIconEx, GetForegroundWindow, GetIconInfo, GetWindowThreadProcessId, DI_NORMAL,
    ICONINFO,
};

// GLOBAL STATE: Store the hash of the clip we just pasted ourselves.
// If the next clipboard change matches this hash, we ignore it (don't update timestamp).
static IGNORE_HASH: Lazy<parking_lot::Mutex<Option<String>>> =
    Lazy::new(|| parking_lot::Mutex::new(None));
static LAST_STABLE_HASH: Lazy<parking_lot::Mutex<Option<String>>> =
    Lazy::new(|| parking_lot::Mutex::new(None));
pub static CLIPBOARD_SYNC: Lazy<Arc<tokio::sync::Mutex<()>>> =
    Lazy::new(|| Arc::new(tokio::sync::Mutex::new(())));

pub fn set_ignore_hash(hash: String) {
    let mut lock = IGNORE_HASH.lock();
    *lock = Some(hash);
}

pub fn init(app: &AppHandle, db: Arc<Database>) {
    crate::ocr_queue::init(db.clone());
    let (snapshot_tx, mut snapshot_rx) = tokio::sync::mpsc::unbounded_channel();
    let app_for_consumer = app.clone();
    let db_for_consumer = db.clone();

    tauri::async_runtime::spawn(async move {
        while let Some(snapshot) = snapshot_rx.recv().await {
            process_clipboard_snapshot(app_for_consumer.clone(), db_for_consumer.clone(), snapshot)
                .await;
        }
        log::error!("CLIPBOARD: Native snapshot queue closed unexpectedly");
    });

    std::thread::Builder::new()
        .name("cubby-clipboard-listener".to_string())
        .spawn(move || run_native_listener(snapshot_tx))
        .unwrap_or_else(|error| panic!("failed to start native clipboard listener: {error}"));
}

type SourceAppInfo = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    bool,
);

#[derive(Clone, Copy)]
struct SourceAppIdentity {
    process_id: u32,
    is_explicit_owner: bool,
}

struct ClipboardImageRead {
    png_bytes: Vec<u8>,
    width: u32,
    height: u32,
    raw_hash: String,
    decode_ms: u128,
    source_type: &'static str,
}

enum CapturedContent {
    Text {
        content: Vec<u8>,
        preview: String,
        hash: String,
    },
    Image {
        png_bytes: Vec<u8>,
        width: u32,
        height: u32,
        hash: String,
        decode_ms: u128,
        source_type: &'static str,
    },
}

struct ClipboardSnapshot {
    sequence: u32,
    source_app_identity: Option<SourceAppIdentity>,
    content: CapturedContent,
    formats: Vec<CapturedFormat>,
    materialize_ms: u128,
    /// The source application tagged this copy as sensitive (e.g. a password
    /// manager) so clipboard monitors should skip it. See `clipboard_marked_sensitive`.
    sensitive: bool,
}

/// Returns true when the current clipboard contents are tagged with the
/// well-known `ExcludeClipboardContentFromMonitorProcessing` format. Password
/// managers and other secret-holding apps set this so clipboard history tools
/// skip the copy. Its mere presence means "do not retain"; reading it does not
/// require opening the clipboard, so this is cheap and contention-free.
#[cfg(target_os = "windows")]
fn clipboard_marked_sensitive() -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::System::DataExchange::{
        IsClipboardFormatAvailable, RegisterClipboardFormatW,
    };

    let name: Vec<u16> = "ExcludeClipboardContentFromMonitorProcessing"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let format = unsafe { RegisterClipboardFormatW(PCWSTR(name.as_ptr())) };
    format != 0 && unsafe { IsClipboardFormatAvailable(format) }.is_ok()
}

pub(crate) struct CapturedFormat {
    pub(crate) name: &'static str,
    pub(crate) content: Vec<u8>,
}

#[cfg(target_os = "windows")]
fn run_native_listener(snapshot_tx: tokio::sync::mpsc::UnboundedSender<ClipboardSnapshot>) {
    let mut monitor = match Monitor::new() {
        Ok(monitor) => monitor,
        Err(error) => {
            log::error!("CLIPBOARD: Failed to create native listener: {}", error);
            return;
        }
    };

    log::info!("CLIPBOARD: Native WM_CLIPBOARDUPDATE listener started");

    loop {
        match monitor.recv() {
            Ok(true) => {
                let started = std::time::Instant::now();
                let sequence =
                    unsafe { windows::Win32::System::DataExchange::GetClipboardSequenceNumber() };
                let source_app_identity = get_clipboard_owner_identity();
                let sensitive = clipboard_marked_sensitive();

                if let Some((content, formats)) = materialize_clipboard_content() {
                    let snapshot = ClipboardSnapshot {
                        sequence,
                        source_app_identity,
                        content,
                        formats,
                        materialize_ms: started.elapsed().as_millis(),
                        sensitive,
                    };

                    if snapshot_tx.send(snapshot).is_err() {
                        log::error!("CLIPBOARD: Snapshot consumer stopped");
                        return;
                    }
                } else {
                    log::debug!(
                        "CLIPBOARD: Sequence {} contained no supported text or image payload",
                        sequence
                    );
                }
            }
            Ok(false) => {
                log::warn!("CLIPBOARD: Native listener received shutdown");
                return;
            }
            Err(error) => {
                log::error!("CLIPBOARD: Native listener failed: {}", error);
                return;
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn run_native_listener(_snapshot_tx: tokio::sync::mpsc::UnboundedSender<ClipboardSnapshot>) {}

fn materialize_clipboard_content() -> Option<(CapturedContent, Vec<CapturedFormat>)> {
    const ATTEMPTS: u32 = 10;

    for attempt in 0..ATTEMPTS {
        if let Ok(ctx) = ClipboardContext::new() {
            if let Ok(files) = ctx.get_files() {
                if !files.is_empty() {
                    let serialized = serde_json::to_vec(&files).ok()?;
                    let preview = files.join("\n").chars().take(200).collect::<String>();
                    let hash = calculate_hash(&serialized);
                    return Some((
                        CapturedContent::Text {
                            content: files.join("\n").into_bytes(),
                            preview,
                            hash,
                        },
                        vec![CapturedFormat {
                            name: "files",
                            content: serialized,
                        }],
                    ));
                }
            }
        }

        if let Ok(ctx) = ClipboardContext::new() {
            if let Ok(text) = ctx.get_text() {
                if let Some(content) = capture_text(text) {
                    let mut formats = Vec::new();
                    if let Ok(html) = ctx.get_html() {
                        if !html.is_empty() {
                            formats.push(CapturedFormat {
                                name: "html",
                                content: html.into_bytes(),
                            });
                        }
                    }
                    if let Ok(rtf) = ctx.get_rich_text() {
                        if !rtf.is_empty() {
                            formats.push(CapturedFormat {
                                name: "rtf",
                                content: rtf.into_bytes(),
                            });
                        }
                    }
                    return Some((content, formats));
                }
            }
        }

        if let Ok(image) = read_clipboard_image_fast() {
            return Some((
                CapturedContent::Image {
                    png_bytes: image.png_bytes,
                    width: image.width,
                    height: image.height,
                    hash: image.raw_hash,
                    decode_ms: image.decode_ms,
                    source_type: image.source_type,
                },
                Vec::new(),
            ));
        }

        if attempt + 1 < ATTEMPTS {
            std::thread::sleep(clipboard_retry_delay(attempt));
        }
    }

    None
}

fn clipboard_retry_delay(attempt: u32) -> std::time::Duration {
    std::time::Duration::from_millis(1_u64 << attempt.min(6))
}

fn capture_text(text: String) -> Option<CapturedContent> {
    if text.is_empty() {
        return None;
    }

    let content = text.into_bytes();
    let preview = String::from_utf8_lossy(&content)
        .chars()
        .take(200)
        .collect::<String>();
    let hash = calculate_hash(&content);
    Some(CapturedContent::Text {
        content,
        preview,
        hash,
    })
}

fn read_clipboard_image_with_clipboard_rs(
    source_type: &'static str,
) -> Result<ClipboardImageRead, String> {
    let ctx = ClipboardContext::new().map_err(|e| e.to_string())?;
    let image = ctx.get_image().map_err(|e| e.to_string())?;
    let (width, height) = image.get_size();

    let dynamic_image = image.get_dynamic_image().map_err(|e| e.to_string())?;
    let raw_hash = calculate_hash(dynamic_image.as_bytes());

    let png_bytes = image
        .to_png()
        .map_err(|e| e.to_string())?
        .get_bytes()
        .to_vec();

    Ok(ClipboardImageRead {
        png_bytes,
        width,
        height,
        raw_hash,
        decode_ms: 0,
        source_type,
    })
}

fn read_clipboard_image_fast() -> Result<ClipboardImageRead, String> {
    read_clipboard_image_with_clipboard_rs("clipboard-rs-image")
}

async fn process_clipboard_snapshot(
    app: AppHandle,
    db: Arc<Database>,
    snapshot: ClipboardSnapshot,
) {
    let started = std::time::Instant::now();
    let mut was_existing = false;
    let _guard = CLIPBOARD_SYNC.lock().await;

    let materialize_ms = snapshot.materialize_ms;
    let sequence = snapshot.sequence;
    let sensitive = snapshot.sensitive;
    let source_app_info = resolve_source_app_info(snapshot.source_app_identity);
    let captured_formats = snapshot.formats;
    let has_files = captured_formats.iter().any(|format| format.name == "files");
    let (clip_type, clip_content, clip_preview, _primary_hash, full_image_content, metadata) =
        match snapshot.content {
            CapturedContent::Text {
                content,
                preview,
                hash,
            } => (
                if has_files { "files" } else { "text" },
                content,
                preview,
                hash,
                None,
                if has_files {
                    Some(
                        serde_json::json!({ "file_count": captured_formats.first().and_then(|format| serde_json::from_slice::<Vec<String>>(&format.content).ok()).map(|files| files.len()).unwrap_or(0) })
                            .to_string(),
                    )
                } else {
                    let format_names: Vec<&str> =
                        captured_formats.iter().map(|format| format.name).collect();
                    (!format_names.is_empty())
                        .then(|| serde_json::json!({ "formats": format_names }).to_string())
                },
            ),
            CapturedContent::Image {
                png_bytes,
                width,
                height,
                hash,
                decode_ms,
                source_type,
            } => {
                let size_bytes = png_bytes.len();
                let preview_bytes = create_image_preview(&png_bytes).unwrap_or_default();
                log::debug!(
                    "CLIPBOARD: Materialized image sequence={} {}x{} source_type={} png_bytes={} decode_ms={}",
                    sequence,
                    width,
                    height,
                    source_type,
                    size_bytes,
                    decode_ms
                );
                (
                    "image",
                    preview_bytes,
                    "[Image]".to_string(),
                    hash,
                    Some(png_bytes),
                    Some(
                        serde_json::json!({
                            "width": width,
                            "height": height,
                            "format": "png",
                            "size_bytes": size_bytes
                        })
                        .to_string(),
                    ),
                )
            }
        };
    let mut hash_material = Vec::new();
    hash_material.extend_from_slice(clip_type.as_bytes());
    hash_material.push(0);
    hash_material.extend_from_slice(full_image_content.as_deref().unwrap_or(&clip_content));
    for format in &captured_formats {
        hash_material.push(0);
        hash_material.extend_from_slice(format.name.as_bytes());
        hash_material.push(0);
        hash_material.extend_from_slice(&format.content);
    }
    let clip_hash = calculate_hash(&hash_material);

    // Ignore our own clipboard writes. When a clip is pasted or reused from
    // Cubby, the paste path sets this ignore hash and already performed the
    // intended move-to-top bump. Re-capturing our own write here would relabel
    // the clip's source app (to Cubby) and re-bump its timestamp, which is what
    // made reused clips collapse to "1 second ago" with a "Cubby Clipboard"
    // source, so skip processing it entirely.
    {
        let mut lock = IGNORE_HASH.lock();
        if lock.as_deref() == Some(clip_hash.as_str()) {
            // Only consume the marker on a match. Clearing it for an
            // intermediate, non-matching snapshot would lose it before our own
            // write arrives, letting the self-paste be persisted after all.
            lock.take();
            log::info!("CLIPBOARD: Ignoring self-paste (own clipboard write)");
            return;
        }
    }

    // Source app info was captured at event time (before debounce) to avoid race conditions
    let (source_app, source_icon, exe_name, full_path, is_explicit_owner) = source_app_info;
    log::debug!(
        "CLIPBOARD: Source attribution available={} executable available={} explicit={}",
        source_app.is_some(),
        exe_name.is_some(),
        is_explicit_owner
    );

    // Check settings (cached via SettingsManager)
    use crate::settings_manager::SettingsManager;
    use tauri::Manager;
    let manager = app.state::<Arc<SettingsManager>>();
    let settings = manager.get();

    if settings.skip_sensitive && sensitive {
        log::info!("CLIPBOARD: Skipping content the source app marked as sensitive");
        return;
    }

    if settings.ignore_ghost_clips && !is_explicit_owner {
        log::info!("CLIPBOARD: Ignoring ghost clip (unknown owner)");
        return;
    }

    // Check if the app is in the ignore list (Case Insensitive)
    let is_ignored = |name: &str| {
        let name_lower = name.to_lowercase();
        settings
            .ignored_apps
            .iter()
            .any(|app| app.to_lowercase() == name_lower)
    };

    if let Some(ref path) = full_path {
        if is_ignored(path) {
            log::info!("CLIPBOARD: Ignoring content from configured application (path match)");
            return;
        }
    }

    if let Some(ref exe) = exe_name {
        if is_ignored(exe) {
            log::info!(
                "CLIPBOARD: Ignoring content from configured application (executable match)"
            );
            return;
        }
    }

    // Only accepted content participates in consecutive duplicate suppression.
    // An ignored application must not prevent the same content from being captured later.
    {
        let lock = LAST_STABLE_HASH.lock();
        if let Some(ref last_hash) = *lock {
            if last_hash == &clip_hash {
                return;
            }
        }
    }

    // DB Logic
    let pool = &db.pool;
    let storage_hash = db.crypto.keyed_hash(&hash_material);
    let encrypted_content = match db.crypto.encrypt(&clip_content) {
        Ok(content) => content,
        Err(error) => {
            log::error!("CLIPBOARD: Failed to encrypt captured content: {}", error);
            return;
        }
    };
    let encrypted_preview = match db.crypto.encrypt_text(&clip_preview) {
        Ok(preview) => preview,
        Err(error) => {
            log::error!("CLIPBOARD: Failed to encrypt captured preview: {}", error);
            return;
        }
    };
    let encrypted_source_app = match db.crypto.encrypt_optional_text(source_app.as_deref()) {
        Ok(value) => value,
        Err(error) => {
            log::error!("CLIPBOARD: Failed to encrypt source attribution: {}", error);
            return;
        }
    };
    let encrypted_source_icon = match db.crypto.encrypt_optional_text(source_icon.as_deref()) {
        Ok(value) => value,
        Err(error) => {
            log::error!("CLIPBOARD: Failed to encrypt source icon: {}", error);
            return;
        }
    };
    let encrypted_metadata = match db.crypto.encrypt_optional_text(metadata.as_deref()) {
        Ok(value) => value,
        Err(error) => {
            log::error!("CLIPBOARD: Failed to encrypt content metadata: {}", error);
            return;
        }
    };

    let db_lookup_started = std::time::Instant::now();
    let existing_uuid: Option<String> =
        sqlx::query_scalar::<_, String>(r#"SELECT uuid FROM clips WHERE content_hash = ?"#)
            .bind(&storage_hash)
            .fetch_optional(pool)
            .await
            .unwrap_or(None);
    let db_lookup_ms = db_lookup_started.elapsed().as_millis();

    let db_write_started = std::time::Instant::now();
    let (emitted_id, inserted_new) = if let Some(existing_id) = existing_uuid {
        was_existing = true;
        if clip_type == "image" {
            if let Err(error) = sqlx::query(
                r#"
                UPDATE clips
                SET created_at = CURRENT_TIMESTAMP,
                    is_deleted = 0,
                    source_app = ?,
                    source_icon = ?,
                    content = ?,
                    text_preview = ?,
                    metadata = ?,
                    is_thumbnail = 0
                WHERE uuid = ?
                "#,
            )
            .bind(&encrypted_source_app)
            .bind(&encrypted_source_icon)
            .bind(&encrypted_content)
            .bind(&encrypted_preview)
            .bind(encrypted_metadata.clone())
            .bind(&existing_id)
            .execute(pool)
            .await
            {
                log::error!(
                    "CLIPBOARD: Failed to update existing image clip {}: {}",
                    existing_id,
                    error
                );
                return;
            }

            if let Some(full_bytes) = &full_image_content {
                match persist_full_image_file(
                    &db.crypto,
                    &db.image_dir,
                    &existing_id,
                    full_bytes,
                ) {
                    Ok(file_path) => {
                        if let Err(error) = sqlx::query(
                            r#"
                            INSERT OR REPLACE INTO clip_images (clip_uuid, full_content, file_path, file_size, storage_kind, mime_type, created_at)
                            VALUES (?, x'', ?, ?, 'file', 'image/png', CURRENT_TIMESTAMP)
                            "#,
                        )
                        .bind(&existing_id)
                        .bind(&file_path)
                        .bind(full_bytes.len() as i64)
                        .execute(pool)
                        .await
                        {
                            log::error!(
                                "CLIPBOARD: Failed to index image file for existing clip {}: {}",
                                existing_id,
                                error
                            );
                            return;
                        }
                    }
                    Err(e) => {
                        log::error!(
                            "Failed to persist full image file for existing clip {}: {}",
                            existing_id,
                            e
                        );
                    }
                }
            }
        } else {
            if let Err(error) = sqlx::query(r#"UPDATE clips SET created_at = CURRENT_TIMESTAMP, is_deleted = 0, source_app = ?, source_icon = ? WHERE uuid = ?"#)
                .bind(&encrypted_source_app)
                .bind(&encrypted_source_icon)
                .bind(&existing_id)
                .execute(pool)
                .await
            {
                log::error!(
                    "CLIPBOARD: Failed to update existing text clip {}: {}",
                    existing_id,
                    error
                );
                return;
            }
        }
        (existing_id, false)
    } else {
        let clip_uuid = Uuid::new_v4().to_string();

        if let Err(error) = sqlx::query(
            r#"
            INSERT INTO clips (uuid, clip_type, content, text_preview, content_hash, folder_id, is_deleted, is_thumbnail, source_app, source_icon, metadata, ocr_status, created_at, last_accessed)
            VALUES (?, ?, ?, ?, ?, NULL, 0, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
            "#,
        )
        .bind(&clip_uuid)
        .bind(clip_type)
        .bind(&encrypted_content)
        .bind(&encrypted_preview)
        .bind(&storage_hash)
        .bind(false)
        .bind(&encrypted_source_app)
        .bind(&encrypted_source_icon)
        .bind(encrypted_metadata)
        .bind((clip_type == "image").then_some("pending"))
        .execute(pool)
        .await
        {
            log::error!(
                "CLIPBOARD: Failed to insert {} clip for sequence {}: {}",
                clip_type,
                sequence,
                error
            );
            return;
        }

        if clip_type == "image" {
            if let Some(full_bytes) = &full_image_content {
                match persist_full_image_file(
                    &db.crypto,
                    &db.image_dir,
                    &clip_uuid,
                    full_bytes,
                ) {
                    Ok(file_path) => {
                        if let Err(error) = sqlx::query(
                            r#"
                            INSERT OR REPLACE INTO clip_images (clip_uuid, full_content, file_path, file_size, storage_kind, mime_type, created_at)
                            VALUES (?, x'', ?, ?, 'file', 'image/png', CURRENT_TIMESTAMP)
                            "#,
                        )
                        .bind(&clip_uuid)
                        .bind(&file_path)
                        .bind(full_bytes.len() as i64)
                        .execute(pool)
                        .await
                        {
                            log::error!(
                                "CLIPBOARD: Failed to index image file for new clip {}: {}",
                                clip_uuid,
                                error
                            );
                            let _ = sqlx::query(r#"DELETE FROM clips WHERE uuid = ?"#)
                                .bind(&clip_uuid)
                                .execute(pool)
                                .await;
                            remove_full_image_file(&file_path);
                            return;
                        }
                    }
                    Err(e) => {
                        log::error!(
                            "Failed to persist full image file for new clip {}, dropping clip: {}",
                            clip_uuid,
                            e
                        );
                        let _ = sqlx::query(r#"DELETE FROM clips WHERE uuid = ?"#)
                            .bind(&clip_uuid)
                            .execute(pool)
                            .await;
                        return;
                    }
                }
            }
        }
        (clip_uuid, true)
    };
    let db_write_ms = db_write_started.elapsed().as_millis();

    // Durable background OCR is queued only after the image payload is safely
    // stored. Re-copying an image with missing OCR also gives it a fresh retry.
    if clip_type == "image" {
        if let Err(error) = crate::ocr_queue::enqueue(&db, &emitted_id).await {
            log::warn!("OCR: could not queue stored image: {error}");
        }
    }

    if let Err(error) = replace_clip_formats(pool, &db.crypto, &emitted_id, &captured_formats).await
    {
        log::error!("CLIPBOARD: Failed to persist auxiliary formats: {}", error);
        if inserted_new {
            let image_path: Option<String> =
                sqlx::query_scalar("SELECT file_path FROM clip_images WHERE clip_uuid = ?")
                    .bind(&emitted_id)
                    .fetch_optional(pool)
                    .await
                    .unwrap_or(None);
            match sqlx::query("DELETE FROM clips WHERE uuid = ?")
                .bind(&emitted_id)
                .execute(pool)
                .await
            {
                Ok(_) => crate::commands::remove_clip_image_files(
                    &db.image_dir,
                    image_path.into_iter().collect(),
                ),
                Err(cleanup_error) => log::error!(
                    "CLIPBOARD: Failed to roll back incomplete clip {}: {}",
                    emitted_id,
                    cleanup_error
                ),
            }
        }
        return;
    }

    *LAST_STABLE_HASH.lock() = Some(clip_hash.clone());

    let retention_deleted = match crate::commands::enforce_retention_in_pool(
        pool,
        settings.max_items,
        settings.auto_delete_days,
    )
    .await
    {
        Ok((deleted, image_paths)) => {
            crate::commands::remove_clip_image_files(&db.image_dir, image_paths);
            if deleted > 0 {
                log::info!(
                    "CLIPBOARD: Retention removed {} expired or overflow items",
                    deleted
                );
            }
            deleted
        }
        Err(error) => {
            log::error!("CLIPBOARD: Retention maintenance failed: {}", error);
            0
        }
    };

    if retention_deleted > 0 {
        db.search_index.invalidate();
    } else {
        db.search_index
            .upsert(&emitted_id, clip_type, &clip_content, &clip_preview, None);
    }

    let emit_started = std::time::Instant::now();
    let _ = app.emit(
        "clipboard-change",
        &serde_json::json!({
            "id": emitted_id,
            "content": clip_preview,
            "clip_type": clip_type,
            "source_app": source_app,
            "source_icon": source_icon,
            "created_at": chrono::Utc::now().to_rfc3339()
        }),
    );
    let emit_ms = emit_started.elapsed().as_millis();

    log::info!(
        "[perf][clipboard_ingest] sequence={} type={} existing={} full_bytes={} thumb_bytes={} materialize_ms={} db_lookup_ms={} db_write_ms={} emit_ms={} total_ms={}",
        sequence,
        clip_type,
        was_existing,
        full_image_content.as_ref().map(|v| v.len()).unwrap_or(0),
        if clip_type == "image" { clip_content.len() } else { 0 },
        materialize_ms,
        db_lookup_ms,
        db_write_ms,
        emit_ms,
        started.elapsed().as_millis()
    );
}
pub(crate) fn calculate_hash(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    let result = hasher.finalize();
    format!("{:x}", result)
}

pub(crate) async fn replace_clip_formats(
    pool: &sqlx::SqlitePool,
    crypto: &crate::crypto::CryptoManager,
    clip_uuid: &str,
    formats: &[CapturedFormat],
) -> Result<(), String> {
    let mut transaction = pool.begin().await.map_err(|e| e.to_string())?;
    sqlx::query("DELETE FROM clip_formats WHERE clip_uuid = ?")
        .bind(clip_uuid)
        .execute(&mut *transaction)
        .await
        .map_err(|e| e.to_string())?;
    for format in formats {
        sqlx::query("INSERT INTO clip_formats (clip_uuid, format, content) VALUES (?, ?, ?)")
            .bind(clip_uuid)
            .bind(format.name)
            .bind(crypto.encrypt(&format.content)?)
            .execute(&mut *transaction)
            .await
            .map_err(|e| e.to_string())?;
    }
    transaction.commit().await.map_err(|e| e.to_string())?;
    Ok(())
}

pub fn create_image_preview(png_bytes: &[u8]) -> Result<Vec<u8>, String> {
    let image = image::load_from_memory(png_bytes).map_err(|e| e.to_string())?;
    let preview = image.thumbnail(320, 220);
    let mut bytes = std::io::Cursor::new(Vec::new());
    preview
        .write_to(&mut bytes, image::ImageOutputFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(bytes.into_inner())
}

pub fn persist_full_image_file(
    crypto: &crate::crypto::CryptoManager,
    image_dir: &std::path::Path,
    clip_uuid: &str,
    png_bytes: &[u8],
) -> Result<String, String> {
    std::fs::create_dir_all(image_dir).map_err(|e| e.to_string())?;
    let file_path = image_dir.join(format!("{}.cubby", clip_uuid));
    let encrypted = crypto.encrypt(png_bytes)?;
    std::fs::write(&file_path, encrypted).map_err(|e| e.to_string())?;
    Ok(file_path.to_string_lossy().to_string())
}

pub fn read_full_image_file(
    crypto: &crate::crypto::CryptoManager,
    file_path: &str,
) -> Result<Vec<u8>, String> {
    let encrypted = std::fs::read(file_path).map_err(|e| e.to_string())?;
    crypto.decrypt(&encrypted)
}

pub fn remove_full_image_file(file_path: &str) {
    if let Err(e) = std::fs::remove_file(file_path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            log::warn!("Failed to delete a stored clipboard image: {}", e);
        }
    }
}

#[cfg(target_os = "windows")]
fn get_clipboard_owner_identity() -> Option<SourceAppIdentity> {
    unsafe {
        let (hwnd, is_explicit) = match GetClipboardOwner() {
            Ok(h) if !h.0.is_null() => (h, true),
            Err(e) => {
                log::info!(
                    "CLIPBOARD: GetClipboardOwner failed: {:?}, falling back to foreground window",
                    e
                );
                (GetForegroundWindow(), false)
            }
            Ok(_) => {
                log::info!(
                    "CLIPBOARD: GetClipboardOwner returned null, falling back to foreground window"
                );
                (GetForegroundWindow(), false)
            }
        };

        if hwnd.0.is_null() {
            return None;
        }

        let mut process_id = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut process_id));

        if process_id == 0 {
            return None;
        }

        Some(SourceAppIdentity {
            process_id,
            is_explicit_owner: is_explicit,
        })
    }
}

#[cfg(not(target_os = "windows"))]
fn get_clipboard_owner_identity() -> Option<SourceAppIdentity> {
    None
}

#[cfg(target_os = "windows")]
fn resolve_source_app_info(identity: Option<SourceAppIdentity>) -> SourceAppInfo {
    unsafe {
        let Some(identity) = identity else {
            return (None, None, None, None, false);
        };

        let process_handle = match OpenProcess(
            PROCESS_QUERY_INFORMATION | PROCESS_VM_READ,
            false,
            identity.process_id,
        ) {
            Ok(h) => h,
            Err(_) => return (None, None, None, None, false),
        };

        let mut name_buffer = [0u16; MAX_PATH as usize];
        let name_size = GetModuleBaseNameW(process_handle, None, &mut name_buffer);
        let exe_name = if name_size > 0 {
            String::from_utf16_lossy(&name_buffer[..name_size as usize])
        } else {
            String::new()
        };

        let mut path_buffer = [0u16; MAX_PATH as usize];
        let path_size = GetModuleFileNameExW(Some(process_handle), None, &mut path_buffer);
        let (app_name, app_icon, full_path) = if path_size > 0 {
            let full_path_str = String::from_utf16_lossy(&path_buffer[..path_size as usize]);

            let desc = get_app_description(&full_path_str);
            let final_name = if let Some(d) = desc {
                Some(d)
            } else {
                if !exe_name.is_empty() {
                    Some(exe_name.clone())
                } else {
                    None
                }
            };

            let icon = extract_icon(&full_path_str);
            (final_name, icon, Some(full_path_str))
        } else {
            (
                if !exe_name.is_empty() {
                    Some(exe_name.clone())
                } else {
                    None
                },
                None,
                None,
            )
        };

        let exe_val = if !exe_name.is_empty() {
            Some(exe_name)
        } else {
            None
        };
        (
            app_name,
            app_icon,
            exe_val,
            full_path,
            identity.is_explicit_owner,
        )
    }
}

#[cfg(not(target_os = "windows"))]
fn resolve_source_app_info(_identity: Option<SourceAppIdentity>) -> SourceAppInfo {
    (None, None, None, None, false)
}

#[cfg(target_os = "windows")]
unsafe fn get_app_description(path: &str) -> Option<String> {
    use std::ffi::c_void;

    let wide_path: Vec<u16> = OsStr::new(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let size = GetFileVersionInfoSizeW(windows::core::PCWSTR(wide_path.as_ptr()), None);
    if size == 0 {
        return None;
    }

    let mut data = vec![0u8; size as usize];
    if GetFileVersionInfoW(
        windows::core::PCWSTR(wide_path.as_ptr()),
        Some(0),
        size,
        data.as_mut_ptr() as *mut _,
    )
    .is_err()
    {
        return None;
    }

    let mut lang_ptr: *mut c_void = std::ptr::null_mut();
    let mut lang_len: u32 = 0;

    let translation_query = OsStr::new("\\VarFileInfo\\Translation")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<u16>>();

    if !VerQueryValueW(
        data.as_ptr() as *const _,
        windows::core::PCWSTR(translation_query.as_ptr()),
        &mut lang_ptr,
        &mut lang_len,
    )
    .as_bool()
    {
        return None;
    }

    if lang_len < 4 {
        return None;
    }

    let pairs = std::slice::from_raw_parts(lang_ptr as *const u16, (lang_len / 2) as usize);
    let num_pairs = (lang_len / 4) as usize;

    let mut lang_code = pairs[0];
    let mut charset_code = pairs[1];

    for i in 0..num_pairs {
        let code = pairs[i * 2];
        let charset = pairs[i * 2 + 1];

        if code == 0x0804 {
            lang_code = code;
            charset_code = charset;
        }
    }

    let keys = ["FileDescription", "ProductName"];

    for key in keys {
        let query_str = format!(
            "\\StringFileInfo\\{:04x}{:04x}\\{}",
            lang_code, charset_code, key
        );
        let query = OsStr::new(&query_str)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>();

        let mut desc_ptr: *mut c_void = std::ptr::null_mut();
        let mut desc_len: u32 = 0;

        if VerQueryValueW(
            data.as_ptr() as *const _,
            windows::core::PCWSTR(query.as_ptr()),
            &mut desc_ptr,
            &mut desc_len,
        )
        .as_bool()
        {
            let desc = std::slice::from_raw_parts(desc_ptr as *const u16, desc_len as usize);
            let len = if desc.last() == Some(&0) {
                desc.len() - 1
            } else {
                desc.len()
            };
            if len > 0 {
                return Some(String::from_utf16_lossy(&desc[..len]));
            }
        }
    }

    None
}

#[cfg(target_os = "windows")]
unsafe fn extract_icon(path: &str) -> Option<String> {
    use image::ImageEncoder;

    let wide_path: Vec<u16> = OsStr::new(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut shfi = SHFILEINFOW::default();

    SHGetFileInfoW(
        windows::core::PCWSTR(wide_path.as_ptr()),
        windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL,
        Some(&mut shfi as *mut _),
        std::mem::size_of::<SHFILEINFOW>() as u32,
        SHGFI_ICON | SHGFI_LARGEICON | SHGFI_USEFILEATTRIBUTES,
    );

    if shfi.hIcon.is_invalid() {
        return None;
    }

    let icon = shfi.hIcon;
    struct IconGuard(windows::Win32::UI::WindowsAndMessaging::HICON);
    impl Drop for IconGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = DestroyIcon(self.0);
            }
        }
    }
    let _guard = IconGuard(icon);

    let mut icon_info = ICONINFO::default();
    if GetIconInfo(icon, &mut icon_info).is_err() {
        return None;
    }

    struct BitmapGuard(HBITMAP);
    impl Drop for BitmapGuard {
        fn drop(&mut self) {
            unsafe {
                if !self.0.is_invalid() {
                    let _ = DeleteObject(self.0.into());
                }
            }
        }
    }
    let _bm_mask = BitmapGuard(icon_info.hbmMask);
    let _bm_color = BitmapGuard(icon_info.hbmColor);

    let mut bm = BITMAP::default();
    if GetObjectW(
        icon_info.hbmMask.into(),
        std::mem::size_of::<BITMAP>() as i32,
        Some(&mut bm as *mut _ as *mut _),
    ) == 0
    {
        return None;
    }

    let width = bm.bmWidth;
    let height = if !icon_info.hbmColor.is_invalid() {
        bm.bmHeight
    } else {
        bm.bmHeight / 2
    };

    let screen_dc = GetDC(None);
    let mem_dc = CreateCompatibleDC(Some(screen_dc));
    let mem_bm = CreateCompatibleBitmap(screen_dc, width, height);

    let old_obj = SelectObject(mem_dc, mem_bm.into());

    let _ = DrawIconEx(mem_dc, 0, 0, icon, width, height, 0, None, DI_NORMAL);

    let bi = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: width,
        biHeight: -height,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        ..Default::default()
    };

    let mut pixels = vec![0u8; (width * height * 4) as usize];

    GetDIBits(
        mem_dc,
        mem_bm,
        0,
        height as u32,
        Some(pixels.as_mut_ptr() as *mut _),
        &mut BITMAPINFO {
            bmiHeader: bi,
            ..Default::default()
        },
        DIB_RGB_COLORS,
    );

    SelectObject(mem_dc, old_obj);
    let _ = DeleteDC(mem_dc);
    let _ = DeleteObject(mem_bm.into());
    let _ = ReleaseDC(None, screen_dc);

    for chunk in pixels.chunks_exact_mut(4) {
        let b = chunk[0];
        let r = chunk[2];
        chunk[0] = r;
        chunk[2] = b;
    }

    let mut png_data = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut png_data);
    encoder
        .write_image(
            &pixels,
            width as u32,
            height as u32,
            image::ColorType::Rgba8,
        )
        .ok()?;

    Some(BASE64.encode(&png_data))
}

#[cfg(test)]
mod tests {
    use super::{calculate_hash, capture_text, clipboard_retry_delay, CapturedContent};

    #[test]
    fn capture_text_preserves_exact_whitespace() {
        let original = "  copied text\r\nwith trailing space  ".to_string();
        let captured = capture_text(original.clone()).expect("text should be captured");

        match captured {
            CapturedContent::Text {
                content,
                preview,
                hash,
            } => {
                assert_eq!(content, original.as_bytes());
                assert_eq!(preview, original);
                assert_eq!(hash, calculate_hash(original.as_bytes()));
            }
            CapturedContent::Image { .. } => panic!("expected text"),
        }
    }

    #[test]
    fn capture_text_ignores_only_truly_empty_content() {
        assert!(capture_text(String::new()).is_none());
        assert!(capture_text("   ".to_string()).is_some());
    }

    #[test]
    fn clipboard_contention_backoff_is_bounded() {
        let delays = (0..10)
            .map(|attempt| clipboard_retry_delay(attempt).as_millis())
            .collect::<Vec<_>>();

        assert_eq!(delays, vec![1, 2, 4, 8, 16, 32, 64, 64, 64, 64]);
        assert_eq!(delays.iter().sum::<u128>(), 319);
    }
}
