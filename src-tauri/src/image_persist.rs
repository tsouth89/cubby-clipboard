use crate::crypto::CryptoManager;
use std::path::{Path, PathBuf};

/// A full-resolution original that is already written and encrypted on disk as
/// `{uuid}.cubby.tmp` but has not yet taken the place of the live
/// `{uuid}.cubby`. Nothing observes the new bytes until `commit` runs, so every
/// early return between staging and commit leaves the previous original
/// byte-identical. Dropping the handle without committing removes the temp file.
pub(crate) struct StagedImageFile {
    temp_path: PathBuf,
    final_path: PathBuf,
    committed: bool,
}

impl StagedImageFile {
    /// The path the original will occupy once `commit` succeeds. This is what
    /// `clip_images.file_path` must record, because the commit only ever moves
    /// the staged bytes onto exactly this name.
    pub(crate) fn final_path(&self) -> String {
        self.final_path.to_string_lossy().to_string()
    }

    /// Replace the live original with the staged bytes.
    pub(crate) fn commit(mut self) -> Result<String, String> {
        replace_image_file_atomically(&self.temp_path, &self.final_path)?;
        self.committed = true;
        Ok(self.final_path.to_string_lossy().to_string())
    }
}

impl Drop for StagedImageFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = std::fs::remove_file(&self.temp_path);
        }
    }
}

/// Encrypt and write the new original to a sibling temp file without touching
/// the live `{uuid}.cubby`. `std::fs::write` truncates on open, so writing the
/// live name in place would destroy the previous good file if the write failed
/// mid-stream.
pub(crate) fn stage_full_image_file(
    crypto: &CryptoManager,
    image_dir: &Path,
    clip_uuid: &str,
    png_bytes: &[u8],
) -> Result<StagedImageFile, String> {
    std::fs::create_dir_all(image_dir).map_err(|e| e.to_string())?;
    let final_path = image_dir.join(format!("{}.cubby", clip_uuid));
    let temp_path = image_dir.join(format!("{}.cubby.tmp", clip_uuid));
    let encrypted = crypto.encrypt(png_bytes)?;
    std::fs::write(&temp_path, encrypted).map_err(|e| e.to_string())?;
    Ok(StagedImageFile {
        temp_path,
        final_path,
        committed: false,
    })
}

/// Stage and immediately commit a full original. Used by the paths that write a
/// brand-new `{uuid}.cubby` (a new clip, the encryption migration), where there
/// is no previous original that a failed follow-up step could destroy.
pub fn persist_full_image_file(
    crypto: &CryptoManager,
    image_dir: &Path,
    clip_uuid: &str,
    png_bytes: &[u8],
) -> Result<String, String> {
    stage_full_image_file(crypto, image_dir, clip_uuid, png_bytes)?.commit()
}

#[cfg(target_os = "windows")]
fn replace_image_file_atomically(source: &Path, destination: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source_wide = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination_wide = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();

    unsafe {
        MoveFileExW(
            PCWSTR(source_wide.as_ptr()),
            PCWSTR(destination_wide.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|error| error.to_string())
}

#[cfg(not(target_os = "windows"))]
fn replace_image_file_atomically(source: &Path, destination: &Path) -> Result<(), String> {
    std::fs::rename(source, destination).map_err(|error| error.to_string())
}

/// The already-encrypted column values a recapture writes back onto the
/// existing `clips` row. Grouped so the recapture entry points stay under the
/// argument-count lint instead of threading six loose encrypted blobs through.
pub(crate) struct RecaptureFields<'a> {
    pub source_app: &'a Option<String>,
    pub source_icon: &'a Option<String>,
    pub content: &'a [u8],
    pub preview: &'a str,
    pub metadata: Option<String>,
}

/// Apply a recapture after (or instead of) a staging attempt. Passing `Err`
/// leaves the previous `clips` row, `clip_images` index, and on-disk original
/// untouched so tests can pin the staging-failure path without AppHandle.
///
/// Ordering matters: both statements run inside one transaction, and the staged
/// original only replaces the live file after that transaction commits. A
/// failure at any earlier point returns with the staged handle un-committed, so
/// its `Drop` deletes the temp file and the previous original survives intact.
pub(crate) async fn apply_existing_image_recapture(
    pool: &sqlx::SqlitePool,
    existing_id: &str,
    staged: Result<StagedImageFile, String>,
    full_bytes_len: i64,
    fields: RecaptureFields<'_>,
) -> Result<(), String> {
    let staged = staged.map_err(|error| {
        format!("Failed to persist full image file for existing clip {existing_id}: {error}")
    })?;
    let file_path = staged.final_path();

    let mut transaction = pool.begin().await.map_err(|error| {
        format!("CLIPBOARD: Failed to start the recapture transaction for existing clip {existing_id}: {error}")
    })?;

    sqlx::query(
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
    .bind(fields.source_app)
    .bind(fields.source_icon)
    .bind(fields.content)
    .bind(fields.preview)
    .bind(fields.metadata)
    .bind(existing_id)
    .execute(&mut *transaction)
    .await
    .map_err(|error| {
        format!("CLIPBOARD: Failed to update existing image clip {existing_id}: {error}")
    })?;

    sqlx::query(
        r#"
        INSERT OR REPLACE INTO clip_images (clip_uuid, full_content, file_path, file_size, storage_kind, mime_type, created_at)
        VALUES (?, x'', ?, ?, 'file', 'image/png', CURRENT_TIMESTAMP)
        "#,
    )
    .bind(existing_id)
    .bind(&file_path)
    .bind(full_bytes_len)
    .execute(&mut *transaction)
    .await
    .map_err(|error| {
        format!("CLIPBOARD: Failed to index image file for existing clip {existing_id}: {error}")
    })?;

    transaction.commit().await.map_err(|error| {
        format!(
            "CLIPBOARD: Failed to commit the recapture for existing clip {existing_id}: {error}"
        )
    })?;

    // The database now describes the staged bytes, so replacing the previous
    // original is the last step. If this single rename fails the previous
    // original is still the one on disk; that leaves the row's thumbnail and
    // file_size ahead of the file, which is reported as an error rather than
    // silently losing the original.
    staged.commit().map_err(|error| {
        format!("CLIPBOARD: Failed to replace the stored original for existing clip {existing_id}: {error}")
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        apply_existing_image_recapture, persist_full_image_file, stage_full_image_file,
        RecaptureFields, StagedImageFile,
    };
    use crate::crypto::CryptoManager;
    use sqlx::sqlite::SqlitePoolOptions;

    fn read_original(crypto: &CryptoManager, path: &str) -> Vec<u8> {
        let encrypted = std::fs::read(path).expect("original file should exist");
        crypto
            .decrypt(&encrypted)
            .expect("original file should still decrypt")
    }

    fn new_fields<'a>(content: &'a [u8], preview: &'a str) -> RecaptureFields<'a> {
        RecaptureFields {
            source_app: &None,
            source_icon: &None,
            content,
            preview,
            metadata: None,
        }
    }

    #[test]
    fn persist_full_image_file_does_not_truncate_the_live_original_when_the_temp_write_fails() {
        let crypto = CryptoManager::ephemeral();
        let image_dir =
            std::env::temp_dir().join(format!("cubby-image-persist-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&image_dir).unwrap();
        let uuid = "existing-original";
        let live_path =
            persist_full_image_file(&crypto, &image_dir, uuid, b"previous-full-res").unwrap();
        assert_eq!(read_original(&crypto, &live_path), b"previous-full-res");

        // Occupy the staging name with a directory so `fs::write` cannot
        // create the temp file. The live `{uuid}.cubby` must stay intact.
        let temp_path = image_dir.join(format!("{uuid}.cubby.tmp"));
        std::fs::create_dir(&temp_path).unwrap();

        let persist_err = persist_full_image_file(&crypto, &image_dir, uuid, b"new-full-res");
        assert!(
            persist_err.is_err(),
            "staging-path collision should fail the persist"
        );
        assert_eq!(
            read_original(&crypto, &live_path),
            b"previous-full-res",
            "a failed persist must not truncate or replace the previous original"
        );

        let _ = std::fs::remove_dir_all(&image_dir);
    }

    #[test]
    fn dropping_a_staged_file_without_committing_removes_the_temp_and_keeps_the_original() {
        let crypto = CryptoManager::ephemeral();
        let image_dir =
            std::env::temp_dir().join(format!("cubby-image-staged-{}", uuid::Uuid::new_v4()));
        let uuid = "staged-but-abandoned";
        let live_path =
            persist_full_image_file(&crypto, &image_dir, uuid, b"previous-full-res").unwrap();

        let temp_path = image_dir.join(format!("{uuid}.cubby.tmp"));
        {
            let staged = stage_full_image_file(&crypto, &image_dir, uuid, b"new-full-res").unwrap();
            assert!(temp_path.exists(), "staging should write the temp file");
            assert_eq!(staged.final_path(), live_path);
        }

        assert!(
            !temp_path.exists(),
            "an un-committed staged file must clean up its temp file"
        );
        assert_eq!(read_original(&crypto, &live_path), b"previous-full-res");

        let _ = std::fs::remove_dir_all(&image_dir);
    }

    async fn recapture_pool() -> sqlx::SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory database should open");
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query(
            r#"
            CREATE TABLE clips (
                uuid TEXT PRIMARY KEY,
                clip_type TEXT NOT NULL,
                content BLOB NOT NULL,
                text_preview TEXT,
                content_hash TEXT NOT NULL,
                is_deleted INTEGER NOT NULL DEFAULT 0,
                is_thumbnail INTEGER NOT NULL DEFAULT 0,
                source_app TEXT,
                source_icon TEXT,
                metadata TEXT,
                ocr_status TEXT,
                ocr_text TEXT,
                full_image_expired INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                last_accessed TEXT NOT NULL
            )
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            CREATE TABLE clip_images (
                clip_uuid TEXT PRIMARY KEY,
                full_content BLOB NOT NULL,
                file_path TEXT,
                file_size INTEGER,
                storage_kind TEXT NOT NULL,
                mime_type TEXT NOT NULL,
                created_at TEXT,
                FOREIGN KEY (clip_uuid) REFERENCES clips(uuid) ON DELETE CASCADE
            )
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    async fn seed_healthy_image_clip(
        pool: &sqlx::SqlitePool,
        crypto: &CryptoManager,
        image_dir: &std::path::Path,
        uuid: &str,
        original_png: &[u8],
    ) -> (Vec<u8>, String, String) {
        let file_path = persist_full_image_file(crypto, image_dir, uuid, original_png).unwrap();
        let content = crypto.encrypt(b"old-thumbnail").unwrap();
        let preview = crypto.encrypt_text("[Image]").unwrap();
        sqlx::query(
            r#"
            INSERT INTO clips (
                uuid, clip_type, content, text_preview, content_hash,
                is_deleted, is_thumbnail, ocr_status, ocr_text,
                full_image_expired, created_at, last_accessed
            )
            VALUES (
                ?, 'image', ?, ?, 'existing-hash',
                0, 0, 'completed', 'already-recognized',
                0, '2026-01-15 12:00:00', '2026-01-15 12:00:00'
            )
            "#,
        )
        .bind(uuid)
        .bind(&content)
        .bind(&preview)
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(
            r#"
            INSERT INTO clip_images (
                clip_uuid, full_content, file_path, file_size, storage_kind, mime_type
            )
            VALUES (?, x'', ?, ?, 'file', 'image/png')
            "#,
        )
        .bind(uuid)
        .bind(&file_path)
        .bind(original_png.len() as i64)
        .execute(pool)
        .await
        .unwrap();
        (content, preview, file_path)
    }

    type RecaptureRow = (
        Vec<u8>,
        Option<String>,
        i64,
        i64,
        Option<String>,
        Option<String>,
        String,
    );

    async fn load_recapture_row(pool: &sqlx::SqlitePool, uuid: &str) -> RecaptureRow {
        sqlx::query_as(
            r#"
            SELECT content, text_preview, is_thumbnail, full_image_expired,
                   ocr_status, ocr_text, created_at
            FROM clips WHERE uuid = ?
            "#,
        )
        .bind(uuid)
        .fetch_one(pool)
        .await
        .expect("clip row should still exist")
    }

    fn assert_still_prior_healthy_clip(row: &RecaptureRow, old_content: &[u8], old_preview: &str) {
        assert_eq!(row.0, old_content, "thumbnail bytes must stay prior");
        assert_eq!(
            row.1.as_deref(),
            Some(old_preview),
            "preview must stay prior"
        );
        assert_eq!(row.2, 0, "prior clip was already full-resolution");
        assert_eq!(
            row.3, 0,
            "a persist miss must not look like retention expiry"
        );
        assert_eq!(row.4.as_deref(), Some("completed"));
        assert_eq!(row.5.as_deref(), Some("already-recognized"));
        assert_eq!(row.6, "2026-01-15 12:00:00");
    }

    #[tokio::test]
    async fn persist_err_on_an_existing_image_uuid_keeps_the_previous_original() {
        let pool = recapture_pool().await;
        let crypto = CryptoManager::ephemeral();
        let image_dir =
            std::env::temp_dir().join(format!("cubby-recapture-test-{}", uuid::Uuid::new_v4()));
        let uuid = "existing-image-recapture";
        let (old_content, old_preview, file_path) =
            seed_healthy_image_clip(&pool, &crypto, &image_dir, uuid, b"previous-full-res").await;

        let recapture = apply_existing_image_recapture(
            &pool,
            uuid,
            Err::<StagedImageFile, String>("disk full".to_string()),
            99,
            new_fields(b"new-thumbnail", "new-preview"),
        )
        .await;

        assert!(
            recapture.is_err(),
            "persist Err must fail the recapture, not continue as success"
        );
        let error = recapture.unwrap_err();
        assert!(
            error.contains(uuid),
            "the error should name the existing clip: {error}"
        );
        assert!(
            error.contains("persist"),
            "the error should describe the persist failure: {error}"
        );

        let row = load_recapture_row(&pool, uuid).await;
        assert_still_prior_healthy_clip(&row, &old_content, &old_preview);

        let indexed_size: i64 =
            sqlx::query_scalar("SELECT file_size FROM clip_images WHERE clip_uuid = ?")
                .bind(uuid)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(indexed_size, b"previous-full-res".len() as i64);
        assert_eq!(read_original(&crypto, &file_path), b"previous-full-res");

        let pending: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM clips WHERE uuid = ? AND ocr_status = 'pending'",
        )
        .bind(uuid)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(pending, 0, "OCR must not be queued against a missing file");

        let _ = std::fs::remove_dir_all(&image_dir);
    }

    #[tokio::test]
    async fn persist_err_from_a_failed_file_write_does_not_commit_the_recapture() {
        let pool = recapture_pool().await;
        let crypto = CryptoManager::ephemeral();
        let image_dir = std::env::temp_dir().join(format!(
            "cubby-recapture-write-fail-{}",
            uuid::Uuid::new_v4()
        ));
        let uuid = "existing-image-write-fail";
        let (old_content, old_preview, file_path) =
            seed_healthy_image_clip(&pool, &crypto, &image_dir, uuid, b"previous-full-res").await;

        let temp_path = image_dir.join(format!("{uuid}.cubby.tmp"));
        std::fs::create_dir(&temp_path).unwrap();
        let staged = stage_full_image_file(&crypto, &image_dir, uuid, b"new-full-res");
        assert!(staged.is_err());

        let recapture = apply_existing_image_recapture(
            &pool,
            uuid,
            staged,
            b"new-full-res".len() as i64,
            new_fields(b"new-thumbnail", "new-preview"),
        )
        .await;
        assert!(recapture.is_err());

        let row = load_recapture_row(&pool, uuid).await;
        assert_still_prior_healthy_clip(&row, &old_content, &old_preview);
        assert_eq!(read_original(&crypto, &file_path), b"previous-full-res");

        let _ = std::fs::remove_dir_all(&image_dir);
    }

    /// The opposite order of the write-failure test: the new original is
    /// written successfully and only the database write fails. The rename must
    /// not have happened yet, so the previous original is still on disk and the
    /// half-applied `UPDATE clips` is rolled back with it.
    #[tokio::test]
    async fn a_db_failure_after_a_successful_write_keeps_the_previous_original() {
        let pool = recapture_pool().await;
        let crypto = CryptoManager::ephemeral();
        let image_dir =
            std::env::temp_dir().join(format!("cubby-recapture-db-fail-{}", uuid::Uuid::new_v4()));
        let uuid = "existing-image-db-fail";
        let (old_content, old_preview, file_path) =
            seed_healthy_image_clip(&pool, &crypto, &image_dir, uuid, b"previous-full-res").await;

        // Fail the second statement only, after `UPDATE clips` has already
        // succeeded. Without a transaction that leaves the new thumbnail
        // beside the old file_size.
        sqlx::query(
            r#"
            CREATE TRIGGER block_clip_images_insert BEFORE INSERT ON clip_images
            BEGIN
                SELECT RAISE(ABORT, 'injected clip_images failure');
            END
            "#,
        )
        .execute(&pool)
        .await
        .unwrap();

        let staged = stage_full_image_file(&crypto, &image_dir, uuid, b"new-full-res");
        assert!(staged.is_ok(), "the file write itself must succeed here");

        let recapture = apply_existing_image_recapture(
            &pool,
            uuid,
            staged,
            b"new-full-res".len() as i64,
            new_fields(b"new-thumbnail", "new-preview"),
        )
        .await;
        assert!(
            recapture.is_err(),
            "a failed clip_images write must fail the recapture"
        );

        let row = load_recapture_row(&pool, uuid).await;
        assert_still_prior_healthy_clip(&row, &old_content, &old_preview);

        let indexed_size: i64 =
            sqlx::query_scalar("SELECT file_size FROM clip_images WHERE clip_uuid = ?")
                .bind(uuid)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(
            indexed_size,
            b"previous-full-res".len() as i64,
            "clip_images must still describe the file that is actually on disk"
        );
        assert_eq!(
            read_original(&crypto, &file_path),
            b"previous-full-res",
            "a failed database write must not have replaced the previous original"
        );
        assert!(
            !image_dir.join(format!("{uuid}.cubby.tmp")).exists(),
            "the abandoned staged file must be cleaned up"
        );

        let _ = std::fs::remove_dir_all(&image_dir);
    }

    #[tokio::test]
    async fn a_successful_persist_commits_the_recapture_and_replaces_the_original() {
        let pool = recapture_pool().await;
        let crypto = CryptoManager::ephemeral();
        let image_dir =
            std::env::temp_dir().join(format!("cubby-recapture-ok-{}", uuid::Uuid::new_v4()));
        let uuid = "existing-image-success";
        let (_old_content, _old_preview, file_path) =
            seed_healthy_image_clip(&pool, &crypto, &image_dir, uuid, b"previous-full-res").await;

        let staged = stage_full_image_file(&crypto, &image_dir, uuid, b"new-full-res");
        apply_existing_image_recapture(
            &pool,
            uuid,
            staged,
            b"new-full-res".len() as i64,
            new_fields(b"new-thumbnail", "new-preview"),
        )
        .await
        .expect("successful persist should commit the recapture");

        let row = load_recapture_row(&pool, uuid).await;
        assert_eq!(row.0, b"new-thumbnail");
        assert_eq!(row.1.as_deref(), Some("new-preview"));
        assert_eq!(row.2, 0);
        assert_ne!(row.6, "2026-01-15 12:00:00", "timestamp should bump");
        assert_eq!(read_original(&crypto, &file_path), b"new-full-res");

        let indexed_size: i64 =
            sqlx::query_scalar("SELECT file_size FROM clip_images WHERE clip_uuid = ?")
                .bind(uuid)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(indexed_size, b"new-full-res".len() as i64);
        assert!(
            !image_dir.join(format!("{uuid}.cubby.tmp")).exists(),
            "a committed staged file must leave no temp file behind"
        );

        let _ = std::fs::remove_dir_all(&image_dir);
    }
}
