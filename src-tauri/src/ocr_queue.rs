use crate::database::Database;
use once_cell::sync::Lazy;
use serde::Serialize;
use sqlx::{Row, SqlitePool};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tauri::State;
use tokio::sync::Notify;

const MAX_ATTEMPTS: i64 = 3;
const IDLE_POLL_INTERVAL: Duration = Duration::from_secs(30);

static STARTED: AtomicBool = AtomicBool::new(false);
static PAUSED: AtomicBool = AtomicBool::new(false);
static WORK_AVAILABLE: Lazy<Notify> = Lazy::new(Notify::new);

#[derive(Debug, Serialize)]
pub struct OcrQueueStatus {
    pending: i64,
    processing: i64,
    completed: i64,
    failed: i64,
    unavailable: i64,
    paused: bool,
}

#[derive(Debug)]
struct OcrJob {
    clip_uuid: String,
    file_path: Option<String>,
    full_content: Vec<u8>,
    attempts: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OcrFailureKind {
    Unavailable,
    Timeout,
    Canceled,
    Decode,
    ResourceLimit,
    MissingImage,
    Engine,
}

impl OcrFailureKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Timeout => "timeout",
            Self::Canceled => "canceled",
            Self::Decode => "decode",
            Self::ResourceLimit => "resource_limit",
            Self::MissingImage => "missing_image",
            Self::Engine => "engine",
        }
    }

    fn is_retryable(self) -> bool {
        matches!(self, Self::Timeout | Self::Canceled | Self::Engine)
    }
}

pub fn init(db: Arc<Database>) {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }

    tauri::async_runtime::spawn(async move {
        loop {
            if let Err(error) = run_worker(db.clone()).await {
                // A transient database failure must not permanently strand the
                // queue until the next app restart. Keep one bounded supervisor
                // alive and restart the worker after a short delay.
                log::error!("OCR queue stopped unexpectedly; retrying: {error}");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    });
}

pub async fn enqueue(db: &Database, clip_uuid: &str) -> Result<(), String> {
    sqlx::query(
        r#"
        UPDATE clips
        SET ocr_status = 'pending',
            ocr_attempts = 0,
            ocr_next_retry_at = NULL,
            ocr_error_kind = NULL
        WHERE uuid = ?
          AND clip_type = 'image'
          AND ocr_text IS NULL
        "#,
    )
    .bind(clip_uuid)
    .execute(&db.pool)
    .await
    .map_err(|error| error.to_string())?;

    WORK_AVAILABLE.notify_one();
    Ok(())
}

async fn run_worker(db: Arc<Database>) -> Result<(), String> {
    PAUSED.store(load_paused(&db.pool).await?, Ordering::SeqCst);
    recover_processing_jobs(&db.pool).await?;
    loop {
        if PAUSED.load(Ordering::SeqCst) {
            tokio::select! {
                _ = WORK_AVAILABLE.notified() => {},
                _ = tokio::time::sleep(IDLE_POLL_INTERVAL) => {},
            }
            continue;
        }

        if let Some(job) = claim_next(&db.pool).await? {
            process_job(&db, job).await?;
            continue;
        }

        tokio::select! {
            _ = WORK_AVAILABLE.notified() => {},
            _ = tokio::time::sleep(IDLE_POLL_INTERVAL) => {},
        }
    }
}

#[tauri::command]
pub async fn get_ocr_queue_status(db: State<'_, Arc<Database>>) -> Result<OcrQueueStatus, String> {
    let rows = sqlx::query(
        r#"
        SELECT COALESCE(ocr_status, 'pending') AS status, COUNT(*) AS count
        FROM clips
        WHERE clip_type = 'image' AND is_deleted = 0
        GROUP BY COALESCE(ocr_status, 'pending')
        "#,
    )
    .fetch_all(&db.pool)
    .await
    .map_err(|error| error.to_string())?;

    let mut status = OcrQueueStatus {
        pending: 0,
        processing: 0,
        completed: 0,
        failed: 0,
        unavailable: 0,
        paused: PAUSED.load(Ordering::SeqCst),
    };
    for row in rows {
        let name: String = row.try_get("status").map_err(|error| error.to_string())?;
        let count: i64 = row.try_get("count").map_err(|error| error.to_string())?;
        match name.as_str() {
            "pending" | "retry" => status.pending += count,
            "processing" => status.processing += count,
            "completed" => status.completed += count,
            "failed" => status.failed += count,
            "unavailable" => status.unavailable += count,
            _ => status.pending += count,
        }
    }
    Ok(status)
}

#[tauri::command]
pub async fn set_ocr_queue_paused(
    paused: bool,
    db: State<'_, Arc<Database>>,
) -> Result<(), String> {
    sqlx::query(
        r#"
        INSERT INTO settings (key, value) VALUES ('ocr_queue_paused', ?)
        ON CONFLICT(key) DO UPDATE SET value = excluded.value
        "#,
    )
    .bind(if paused { "true" } else { "false" })
    .execute(&db.pool)
    .await
    .map_err(|error| error.to_string())?;
    PAUSED.store(paused, Ordering::SeqCst);
    WORK_AVAILABLE.notify_one();
    Ok(())
}

#[tauri::command]
pub async fn retry_failed_ocr(db: State<'_, Arc<Database>>) -> Result<u64, String> {
    let result = sqlx::query(
        r#"
        UPDATE clips
        SET ocr_status = 'pending',
            ocr_attempts = 0,
            ocr_next_retry_at = NULL,
            ocr_error_kind = NULL
        WHERE clip_type = 'image'
          AND ocr_text IS NULL
          AND ocr_status IN ('failed', 'unavailable')
        "#,
    )
    .execute(&db.pool)
    .await
    .map_err(|error| error.to_string())?;
    WORK_AVAILABLE.notify_one();
    Ok(result.rows_affected())
}

async fn load_paused(pool: &SqlitePool) -> Result<bool, String> {
    let value: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'ocr_queue_paused'")
            .fetch_optional(pool)
            .await
            .map_err(|error| error.to_string())?;
    Ok(value.as_deref() == Some("true"))
}

async fn recover_processing_jobs(pool: &SqlitePool) -> Result<u64, String> {
    let result = sqlx::query(
        r#"
        UPDATE clips
        SET ocr_status = 'retry',
            ocr_next_retry_at = NULL,
            ocr_error_kind = NULL
        WHERE clip_type = 'image'
          AND is_deleted = 0
          AND ocr_text IS NULL
          AND ocr_status = 'processing'
        "#,
    )
    .execute(pool)
    .await
    .map_err(|error| error.to_string())?;

    Ok(result.rows_affected())
}

async fn claim_next(pool: &SqlitePool) -> Result<Option<OcrJob>, String> {
    let mut transaction = pool.begin().await.map_err(|error| error.to_string())?;
    let row = sqlx::query(
        r#"
        SELECT clips.uuid, clip_images.file_path, clip_images.full_content, clips.ocr_attempts
        FROM clips
        LEFT JOIN clip_images ON clip_images.clip_uuid = clips.uuid
        WHERE clips.clip_type = 'image'
          AND clips.is_deleted = 0
          AND clips.ocr_text IS NULL
          AND clips.ocr_status IN ('pending', 'retry')
          AND (clips.ocr_next_retry_at IS NULL OR clips.ocr_next_retry_at <= CURRENT_TIMESTAMP)
        ORDER BY clips.created_at ASC, clips.id ASC
        LIMIT 1
        "#,
    )
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|error| error.to_string())?;

    let Some(row) = row else {
        transaction
            .commit()
            .await
            .map_err(|error| error.to_string())?;
        return Ok(None);
    };

    let clip_uuid: String = row.try_get("uuid").map_err(|error| error.to_string())?;
    let updated = sqlx::query(
        r#"
        UPDATE clips
        SET ocr_status = 'processing',
            ocr_attempts = ocr_attempts + 1,
            ocr_next_retry_at = NULL,
            ocr_error_kind = NULL
        WHERE uuid = ? AND ocr_status IN ('pending', 'retry')
        "#,
    )
    .bind(&clip_uuid)
    .execute(&mut *transaction)
    .await
    .map_err(|error| error.to_string())?;

    if updated.rows_affected() != 1 {
        transaction
            .rollback()
            .await
            .map_err(|error| error.to_string())?;
        return Ok(None);
    }

    let attempts: i64 = row
        .try_get::<i64, _>("ocr_attempts")
        .map_err(|error| error.to_string())?
        + 1;
    let job = OcrJob {
        clip_uuid,
        file_path: row
            .try_get("file_path")
            .map_err(|error| error.to_string())?,
        full_content: row
            .try_get::<Option<Vec<u8>>, _>("full_content")
            .map_err(|error| error.to_string())?
            .unwrap_or_default(),
        attempts,
    };

    transaction
        .commit()
        .await
        .map_err(|error| error.to_string())?;
    Ok(Some(job))
}

async fn process_job(db: &Database, job: OcrJob) -> Result<(), String> {
    let crypto = db.crypto.clone();
    let image_dir = db.image_dir.clone();
    let file_path = job.file_path.clone();
    let full_content = job.full_content.clone();
    let result = tauri::async_runtime::spawn_blocking(move || {
        let image = load_image(&crypto, &image_dir, file_path.as_deref(), &full_content)?;
        crate::ocr::recognize_png(&image)
    })
    .await;

    match result {
        Ok(Ok(text)) => mark_completed(db, &job.clip_uuid, &text).await?,
        Ok(Err(error)) => {
            let kind = classify_failure(&error);
            mark_failed(db, &job, kind).await?;
        }
        Err(error) => {
            log::warn!("OCR: blocking worker could not be joined: {error}");
            mark_failed(db, &job, OcrFailureKind::Engine).await?;
        }
    }

    Ok(())
}

fn load_image(
    crypto: &crate::crypto::CryptoManager,
    image_dir: &std::path::Path,
    file_path: Option<&str>,
    full_content: &[u8],
) -> Result<Vec<u8>, String> {
    if let Some(path) = file_path
        .filter(|path| !path.is_empty())
        .filter(|path| is_managed_image_path(image_dir, path))
    {
        if let Ok(image) = crate::clipboard::read_full_image_file(crypto, path) {
            return Ok(image);
        }
    }

    if full_content.is_empty() {
        return Err("stored image is missing".to_string());
    }

    if crypto.is_encrypted(full_content) {
        crypto
            .decrypt(full_content)
            .map_err(|_| "stored image could not be decrypted".to_string())
    } else {
        Ok(full_content.to_vec())
    }
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

async fn mark_completed(db: &Database, clip_uuid: &str, text: &str) -> Result<(), String> {
    let encrypted = if text.trim().is_empty() {
        None
    } else {
        Some(db.crypto.encrypt_text(text.trim())?)
    };

    sqlx::query(
        r#"
        UPDATE clips
        SET ocr_text = ?,
            ocr_status = 'completed',
            ocr_next_retry_at = NULL,
            ocr_error_kind = NULL
        WHERE uuid = ?
        "#,
    )
    .bind(encrypted)
    .bind(clip_uuid)
    .execute(&db.pool)
    .await
    .map_err(|error| error.to_string())?;
    db.search_index.update_ocr(clip_uuid, text.trim());
    Ok(())
}

async fn mark_failed(db: &Database, job: &OcrJob, kind: OcrFailureKind) -> Result<(), String> {
    if kind == OcrFailureKind::Unavailable {
        sqlx::query(
            r#"
            UPDATE clips
            SET ocr_status = 'unavailable',
                ocr_next_retry_at = NULL,
                ocr_error_kind = ?
            WHERE clip_type = 'image'
              AND ocr_text IS NULL
              AND ocr_status IN ('pending', 'retry', 'processing')
            "#,
        )
        .bind(kind.as_str())
        .execute(&db.pool)
        .await
        .map_err(|error| error.to_string())?;
        return Ok(());
    }

    if kind.is_retryable() && job.attempts < MAX_ATTEMPTS {
        let delay_seconds = retry_delay_seconds(job.attempts);
        let modifier = format!("+{delay_seconds} seconds");
        sqlx::query(
            r#"
            UPDATE clips
            SET ocr_status = 'retry',
                ocr_next_retry_at = datetime('now', ?),
                ocr_error_kind = ?
            WHERE uuid = ?
            "#,
        )
        .bind(modifier)
        .bind(kind.as_str())
        .bind(&job.clip_uuid)
        .execute(&db.pool)
        .await
        .map_err(|error| error.to_string())?;
    } else {
        sqlx::query(
            r#"
            UPDATE clips
            SET ocr_status = 'failed',
                ocr_next_retry_at = NULL,
                ocr_error_kind = ?
            WHERE uuid = ?
            "#,
        )
        .bind(kind.as_str())
        .bind(&job.clip_uuid)
        .execute(&db.pool)
        .await
        .map_err(|error| error.to_string())?;
    }

    Ok(())
}

fn retry_delay_seconds(attempts: i64) -> i64 {
    30 * 2_i64.pow(attempts.saturating_sub(1).min(4) as u32)
}

fn classify_failure(error: &str) -> OcrFailureKind {
    let normalized = error.to_ascii_lowercase();
    if normalized.contains("no ocr language") || normalized.contains("requires windows") {
        OcrFailureKind::Unavailable
    } else if normalized.contains("timed out") {
        OcrFailureKind::Timeout
    } else if normalized.contains("canceled") {
        OcrFailureKind::Canceled
    } else if normalized.contains("safe ocr limit")
        || normalized.contains("too large for safe ocr")
        || normalized.contains("within safe limits")
    {
        OcrFailureKind::ResourceLimit
    } else if normalized.contains("decode") || normalized.contains("decrypted") {
        OcrFailureKind::Decode
    } else if normalized.contains("missing") || normalized.contains("unreadable") {
        OcrFailureKind::MissingImage
    } else {
        OcrFailureKind::Engine
    }
}

#[cfg(test)]
mod tests {
    use super::{
        claim_next, classify_failure, load_paused, mark_completed, recover_processing_jobs,
        retry_delay_seconds, OcrFailureKind,
    };
    use crate::crypto::CryptoManager;
    use crate::database::Database;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::sync::Arc;

    async fn test_database() -> Database {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory database should open");
        let database = Database {
            pool,
            crypto: Arc::new(CryptoManager::ephemeral()),
            image_dir: std::env::temp_dir().join(format!("cubby-ocr-{}", uuid::Uuid::new_v4())),
            search_index: Arc::new(crate::search_index::SearchIndex::default()),
        };
        database.migrate().await.expect("migration should succeed");
        database
    }

    async fn insert_image(database: &Database, uuid: &str, status: Option<&str>) {
        sqlx::query(
            r#"
            INSERT INTO clips (
                uuid, clip_type, content, text_preview, content_hash,
                is_deleted, is_thumbnail, ocr_status
            ) VALUES (?, 'image', x'', '', ?, 0, 0, ?)
            "#,
        )
        .bind(uuid)
        .bind(format!("hash-{uuid}"))
        .bind(status)
        .execute(&database.pool)
        .await
        .expect("image should insert");
        sqlx::query("INSERT INTO clip_images (clip_uuid, full_content) VALUES (?, x'89504E47')")
            .bind(uuid)
            .execute(&database.pool)
            .await
            .expect("image payload should insert");
    }

    #[tokio::test]
    async fn durable_queue_drains_every_pending_image_without_drops() {
        let database = test_database().await;
        for index in 0..10 {
            insert_image(&database, &format!("clip-{index}"), Some("pending")).await;
        }

        let mut processed = 0;
        while let Some(job) = claim_next(&database.pool)
            .await
            .expect("claim should succeed")
        {
            mark_completed(&database, &job.clip_uuid, "")
                .await
                .expect("completion should persist");
            processed += 1;
        }

        assert_eq!(processed, 10);
        let remaining: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM clips WHERE ocr_status != 'completed'")
                .fetch_one(&database.pool)
                .await
                .expect("count should load");
        assert_eq!(remaining, 0);
    }

    #[tokio::test]
    async fn migration_backfills_missing_ocr_and_recovers_interrupted_jobs() {
        let database = test_database().await;
        insert_image(&database, "untracked", None).await;
        insert_image(&database, "interrupted", Some("processing")).await;

        database.migrate().await.expect("migration should rerun");

        let statuses: Vec<String> =
            sqlx::query_scalar("SELECT ocr_status FROM clips ORDER BY uuid")
                .fetch_all(&database.pool)
                .await
                .expect("statuses should load");
        assert_eq!(statuses, vec!["pending", "pending"]);
    }

    #[tokio::test]
    async fn worker_recovery_requeues_claimed_processing_jobs() {
        let database = test_database().await;
        insert_image(&database, "claimed", Some("pending")).await;

        claim_next(&database.pool)
            .await
            .expect("claim should succeed")
            .expect("job should be claimed");
        assert_eq!(recover_processing_jobs(&database.pool).await.unwrap(), 1);

        let status: String =
            sqlx::query_scalar("SELECT ocr_status FROM clips WHERE uuid = 'claimed'")
                .fetch_one(&database.pool)
                .await
                .expect("status should load");
        assert_eq!(status, "retry");
        assert!(claim_next(&database.pool)
            .await
            .expect("claim should succeed")
            .is_some());
    }

    #[tokio::test]
    async fn queue_claims_oldest_pending_image_first() {
        let database = test_database().await;
        insert_image(&database, "newer", Some("pending")).await;
        insert_image(&database, "older", Some("pending")).await;
        sqlx::query("UPDATE clips SET created_at = '2025-01-02 00:00:00' WHERE uuid = 'newer'")
            .execute(&database.pool)
            .await
            .expect("newer timestamp should update");
        sqlx::query("UPDATE clips SET created_at = '2025-01-01 00:00:00' WHERE uuid = 'older'")
            .execute(&database.pool)
            .await
            .expect("older timestamp should update");

        let job = claim_next(&database.pool)
            .await
            .expect("claim should succeed")
            .expect("job should be claimed");
        assert_eq!(job.clip_uuid, "older");
    }

    #[test]
    fn classifies_safe_error_categories_without_persisting_raw_messages() {
        assert_eq!(
            classify_failure("Windows OCR is unavailable (no OCR language installed)"),
            OcrFailureKind::Unavailable
        );
        assert_eq!(
            classify_failure("Windows OCR timed out"),
            OcrFailureKind::Timeout
        );
        assert_eq!(
            classify_failure("stored image is missing or unreadable"),
            OcrFailureKind::MissingImage
        );
        assert_eq!(
            classify_failure("screenshot dimensions 10000x10000 exceed safe OCR limits"),
            OcrFailureKind::ResourceLimit
        );
        assert_eq!(retry_delay_seconds(1), 30);
        assert_eq!(retry_delay_seconds(2), 60);
        assert_eq!(retry_delay_seconds(3), 120);
    }

    #[tokio::test]
    async fn paused_state_is_persisted_in_settings() {
        let database = test_database().await;
        sqlx::query("INSERT INTO settings (key, value) VALUES ('ocr_queue_paused', 'true')")
            .execute(&database.pool)
            .await
            .expect("pause setting should insert");
        assert!(load_paused(&database.pool)
            .await
            .expect("pause setting should load"));
    }
}
