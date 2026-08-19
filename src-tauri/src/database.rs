use sqlx::SqlitePool;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use crate::crypto::CryptoManager;
use crate::search_index::SearchIndex;

/// How often a healthy on-disk history file is snapshotted to `cubby.db.bak`.
/// Startup stays cheap while still giving a recent recovery point after bad
/// migrations or abrupt power loss. Long-running sessions use the same gate
/// so a machine that never quits still gets a daily copy (SBS-771).
const ROLLING_BACKUP_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

/// How often a live session asks whether that copy is stale. Shorter than
/// `ROLLING_BACKUP_MAX_AGE` so a failed refresh retries on the next hour
/// instead of waiting another day, without rewriting a still-fresh backup
/// on every tick.
const ROLLING_BACKUP_CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60);

static ROLLING_BACKUP_SCHEDULER_STARTED: AtomicBool = AtomicBool::new(false);

#[derive(Clone)]
pub struct Database {
    pub pool: SqlitePool,
    pub crypto: Arc<CryptoManager>,
    pub image_dir: PathBuf,
    pub search_index: Arc<SearchIndex>,
}

impl Database {
    /// Errors are `StorageError` rather than `String` so startup can tell a
    /// DPAPI account mismatch (recoverable, explain it) apart from everything
    /// else (a bug or a broken disk). `From<String>` keeps the `?` on each
    /// String-producing step below unchanged.
    /// Opens storage and returns any history-rewrite notices that happened
    /// before the logger exists. The caller must flush those through the
    /// existing `startup_log` buffer (SBS-929); `log::` here is discarded.
    pub async fn new(
        db_path: &str,
    ) -> Result<(Self, Vec<(log::Level, String)>), crate::crypto::StorageError> {
        let path = Path::new(db_path);
        // Fail open for capture: a corrupt history is quarantined and replaced
        // with an empty database rather than blocking the app (SOU-218).
        let notices = prepare_database_file(path).await?;

        let image_dir = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("images");
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true);

        let pool = SqlitePool::connect_with(options)
            .await
            .map_err(|e| format!("failed to open clipboard database: {e}"))?;
        let settings_table_exists: bool = sqlx::query_scalar(
            r#"
            SELECT EXISTS(
                SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'settings'
            )
            "#,
        )
        .fetch_one(&pool)
        .await
        .map_err(|e| format!("failed to inspect clipboard database: {e}"))?;
        let encryption_version = if settings_table_exists {
            sqlx::query_scalar::<_, String>(
                "SELECT value FROM settings WHERE key = 'storage_encryption_version'",
            )
            .fetch_optional(&pool)
            .await
            .map_err(|e| format!("failed to inspect clipboard encryption state: {e}"))?
        } else {
            None
        };
        let crypto = Arc::new(CryptoManager::load_or_create(
            path,
            encryption_version.as_deref() != Some("1"),
        )?);

        let _ = std::fs::create_dir_all(&image_dir);
        Self::sweep_stale_image_temps(&image_dir);

        Ok((
            Self {
                pool,
                crypto,
                image_dir,
                search_index: Arc::new(crate::search_index::SearchIndex::default()),
            },
            notices,
        ))
    }

    /// Staging writes `{uuid}.cubby.tmp` and only `StagedImageFile::drop` removes
    /// it. A crash between the write and that drop leaves the encrypted original
    /// on disk forever — including through Clear all history, which only deletes
    /// paths recorded in `clip_images` (SBS-998).
    fn sweep_stale_image_temps(image_dir: &Path) {
        let Ok(entries) = std::fs::read_dir(image_dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.ends_with(".cubby.tmp") {
                continue;
            }
            match std::fs::remove_file(&path) {
                Ok(()) => log::info!("IMAGE: removed leftover staging file {}", path.display()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => log::warn!(
                    "IMAGE: could not remove leftover staging file {}: {error}",
                    path.display()
                ),
            }
        }
    }

    /// Collapse duplicate `content_hash` rows and make the constraint
    /// structural.
    ///
    /// Deduplication used to rest entirely on the `SELECT uuid FROM clips WHERE
    /// content_hash = ?` lookup succeeding, because the column carried a plain
    /// index. That makes a failed lookup indistinguishable from a genuine miss,
    /// so any caller that collapses the error arm into `None` silently doubles
    /// the user's history. Fixing the call site fixed one caller; the schema
    /// still allowed the bad state for the next one. This closes it.
    ///
    /// Deliberately **not** part of [`Self::migrate`]. The encrypted-storage
    /// and clip-format migrations rewrite `content_hash` for existing rows, so
    /// they can create duplicates that did not exist when the schema was set
    /// up; creating the unique index before they run would fail the upgrade of
    /// exactly the installs that need it. Call this after them.
    ///
    /// The oldest *visible* copy of each hash survives and inherits the pin,
    /// hide state, folder, and note of every row being removed. Unlike
    /// `remove_duplicate_clips`, which refuses to touch pinned rows at all, this
    /// has to collapse the group to exactly one row for the constraint to hold
    /// -- so the pin is moved rather than honoured in place.
    ///
    /// Returns the number of duplicate rows removed so the caller can report
    /// it. Deliberately does not log: this runs before the log plugin is
    /// installed, so anything written here is discarded.
    ///
    /// Returns the number of duplicate rows removed. On failure the whole
    /// reconciliation rolls back and the old non-unique index is left in place:
    /// an unconstrained database that works beats a half-migrated one.
    pub async fn enforce_content_hash_uniqueness(&self) -> Result<u64, String> {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|error| format!("could not start the deduplication: {error}"))?;

        // Read the duplicate list inside the transaction. Read outside it, a
        // write landing between the snapshot and `CREATE UNIQUE INDEX` would
        // leave a duplicate the reconciliation never saw, failing the index and
        // leaving the database unconstrained for that run.
        let duplicated: Vec<String> = sqlx::query_scalar(
            "SELECT content_hash FROM clips GROUP BY content_hash HAVING COUNT(*) > 1",
        )
        .fetch_all(&mut *transaction)
        .await
        .map_err(|error| format!("could not look for duplicate clips: {error}"))?;

        let mut orphaned_images: Vec<String> = Vec::new();
        let mut removed = 0_u64;

        for hash in &duplicated {
            // The oldest *visible* copy wins, matching `remove_duplicate_clips`.
            //
            // `is_deleted ASC` is the load-bearing part. Soft delete keeps the
            // row, so without it a newer soft-deleted duplicate could win and
            // the only copy the user can actually see would be hard-deleted --
            // the clip would vanish from history entirely. `remove_duplicate_clips`
            // documents the same hazard.
            //
            // Among visible rows the lowest id wins, which is the original
            // capture rather than an accidental re-insert, and is exactly the
            // `MIN(id)` rule that command already uses.
            let rows: Vec<DuplicateRow> =
                sqlx::query_as::<_, (String, bool, bool, Option<i64>, Option<String>)>(
                    r#"
                SELECT uuid, is_pinned, is_hidden, folder_id, notes
                FROM clips
                WHERE content_hash = ?
                ORDER BY is_deleted ASC, id ASC
                "#,
                )
                .bind(hash)
                .fetch_all(&mut *transaction)
                .await
                .map_err(|error| format!("could not read a duplicate clip group: {error}"))?
                .into_iter()
                .map(
                    |(uuid, is_pinned, is_hidden, folder_id, notes)| DuplicateRow {
                        uuid,
                        is_pinned,
                        is_hidden,
                        folder_id,
                        notes,
                    },
                )
                .collect();

            let Some((survivor, losers)) = rows.split_first() else {
                continue;
            };
            if losers.is_empty() {
                continue;
            }

            // Carry forward everything the user set by hand. These rows are
            // duplicates by content, but the organising work on them is not
            // duplicated: a note written on one copy exists only there, and
            // losing it because another copy survived would be silent data
            // loss the user cannot undo.
            let pinned = rows.iter().any(|row| row.is_pinned);
            let hidden = rows.iter().any(|row| row.is_hidden);
            let folder = survivor
                .folder_id
                .or_else(|| rows.iter().find_map(|row| row.folder_id));
            let notes = survivor
                .notes
                .clone()
                .filter(|note| !note.trim().is_empty())
                .or_else(|| {
                    rows.iter()
                        .find_map(|row| row.notes.clone().filter(|note| !note.trim().is_empty()))
                });

            sqlx::query(
                "UPDATE clips SET is_pinned = ?, is_hidden = ?, folder_id = ?, notes = ? WHERE uuid = ?",
            )
            .bind(pinned)
            .bind(hidden)
            .bind(folder)
            .bind(notes)
            .bind(&survivor.uuid)
            .execute(&mut *transaction)
            .await
            .map_err(|error| format!("could not merge duplicate clip state: {error}"))?;

            for loser in losers {
                // Child rows cascade, but the image blobs on disk do not, so
                // their paths are collected before the row disappears.
                let paths: Vec<Option<String>> =
                    sqlx::query_scalar("SELECT file_path FROM clip_images WHERE clip_uuid = ?")
                        .bind(&loser.uuid)
                        .fetch_all(&mut *transaction)
                        .await
                        .map_err(|error| {
                            format!("could not read a duplicate clip's images: {error}")
                        })?;
                orphaned_images.extend(paths.into_iter().flatten());

                sqlx::query("DELETE FROM clips WHERE uuid = ?")
                    .bind(&loser.uuid)
                    .execute(&mut *transaction)
                    .await
                    .map_err(|error| format!("could not remove a duplicate clip: {error}"))?;
                removed += 1;
            }
        }

        // Created before the old index is dropped, so a database that still
        // holds duplicates fails here and rolls back with its index intact.
        sqlx::query(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_clips_hash_unique ON clips(content_hash)",
        )
        .execute(&mut *transaction)
        .await
        .map_err(|error| {
            format!("could not make content_hash unique, so the old index was kept: {error}")
        })?;
        sqlx::query("DROP INDEX IF EXISTS idx_clips_hash")
            .execute(&mut *transaction)
            .await
            .map_err(|error| format!("could not drop the old content_hash index: {error}"))?;

        transaction
            .commit()
            .await
            .map_err(|error| format!("could not commit the deduplication: {error}"))?;

        // Only after the rows are durably gone: a rollback must not leave a
        // surviving clip pointing at a file that has been deleted.
        for path in orphaned_images {
            crate::clipboard::remove_full_image_file(&path);
        }

        if removed > 0 {
            // The in-memory index is built from `clips`, so entries for the
            // rows just deleted would otherwise survive as hits pointing at
            // uuids that no longer exist. `remove_duplicate_clips` invalidates
            // for the same reason. At startup the index has not been built yet
            // and this is a no-op; it matters for any later caller.
            self.search_index.invalidate();
        }
        Ok(removed)
    }

    /// Schema setup. Returns notices for history it rewrote (legacy
    /// file-reference rows). Those must be flushed through `startup_log`;
    /// `log::` here is discarded (SBS-929).
    pub async fn migrate(&self) -> Result<Vec<(log::Level, String)>, sqlx::Error> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS folders (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                icon TEXT,
                color TEXT,
                is_system INTEGER DEFAULT 0,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP
            )
        "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS clips (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                uuid TEXT NOT NULL UNIQUE,
                clip_type TEXT NOT NULL,
                content BLOB NOT NULL,
                text_preview TEXT,
                content_hash TEXT NOT NULL,
                folder_id INTEGER REFERENCES folders(id),
                is_deleted INTEGER DEFAULT 0,
                is_pinned INTEGER NOT NULL DEFAULT 0,
                is_thumbnail INTEGER NOT NULL DEFAULT 0,
                source_app TEXT,
                source_icon TEXT,
                metadata TEXT,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                last_accessed DATETIME DEFAULT CURRENT_TIMESTAMP
            )
        "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE INDEX IF NOT EXISTS idx_clips_folder ON clips(folder_id);
        "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE INDEX IF NOT EXISTS idx_clips_created ON clips(created_at);
        "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS settings (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            )
        "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS ignored_apps (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                app_name TEXT NOT NULL UNIQUE
            )
        "#,
        )
        .execute(&self.pool)
        .await?;

        // Backward-compatible schema updates.
        add_column_if_missing(
            &self.pool,
            "ALTER TABLE clips ADD COLUMN is_thumbnail INTEGER NOT NULL DEFAULT 0",
        )
        .await?;
        add_column_if_missing(
            &self.pool,
            "ALTER TABLE clips ADD COLUMN is_pinned INTEGER NOT NULL DEFAULT 0",
        )
        .await?;
        // Encrypted OCR text extracted from screenshot/image clips, so images are
        // findable by their words. NULL until (or unless) OCR runs for a clip.
        add_column_if_missing(&self.pool, "ALTER TABLE clips ADD COLUMN ocr_text TEXT").await?;
        // Encrypted JSON array of per-word bounding boxes for image clips, stored
        // at capture time so search can later highlight matched words on the
        // preview without re-running OCR (SOU-242). NULL when OCR found no words.
        add_column_if_missing(&self.pool, "ALTER TABLE clips ADD COLUMN ocr_words TEXT").await?;
        add_column_if_missing(&self.pool, "ALTER TABLE clips ADD COLUMN ocr_status TEXT").await?;
        add_column_if_missing(
            &self.pool,
            "ALTER TABLE clips ADD COLUMN ocr_attempts INTEGER NOT NULL DEFAULT 0",
        )
        .await?;
        add_column_if_missing(
            &self.pool,
            "ALTER TABLE clips ADD COLUMN ocr_next_retry_at DATETIME",
        )
        .await?;
        add_column_if_missing(
            &self.pool,
            "ALTER TABLE clips ADD COLUMN ocr_error_kind TEXT",
        )
        .await?;
        // A screenshot whose full-resolution image was dropped at the retention
        // cutoff (SOU-244). The encrypted thumbnail (clips.content) and ocr_text
        // survive, so the clip stays browsable and searchable by its words; only
        // the heavy clip_images blob is gone. 0 = full image still present.
        add_column_if_missing(
            &self.pool,
            "ALTER TABLE clips ADD COLUMN full_image_expired INTEGER NOT NULL DEFAULT 0",
        )
        .await?;

        // A user-written note attached to a clip (SOU-588), so something with no
        // memorable text — a UUID, half an address — can still be found later.
        // Encrypted at rest like every other clip field, and NULL for every row
        // that predates the column.
        add_column_if_missing(&self.pool, "ALTER TABLE clips ADD COLUMN notes TEXT").await?;

        // Per-clip "hide from the list" flag (SOU-586). Purely a display state:
        // the content stays encrypted at rest exactly as before and still pastes
        // normally. 0 = shown.
        add_column_if_missing(
            &self.pool,
            "ALTER TABLE clips ADD COLUMN is_hidden INTEGER NOT NULL DEFAULT 0",
        )
        .await?;

        // Existing images without OCR become durable background work. A process
        // that exited while a job was running leaves it as `processing`; reset
        // those jobs so the next launch can recover them.
        sqlx::query(
            r#"
            UPDATE clips
            SET ocr_status = CASE
                    WHEN ocr_text IS NOT NULL THEN 'completed'
                    ELSE 'pending'
                END,
                ocr_next_retry_at = NULL,
                ocr_error_kind = NULL
            WHERE clip_type = 'image'
              AND (ocr_status IS NULL OR ocr_status IN ('processing', 'unavailable'))
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE INDEX IF NOT EXISTS idx_clips_ocr_queue
            ON clips(ocr_status, ocr_next_retry_at, created_at)
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS clip_images (
                clip_uuid TEXT PRIMARY KEY,
                full_content BLOB NOT NULL,
                file_path TEXT,
                file_size INTEGER,
                storage_kind TEXT NOT NULL DEFAULT 'db',
                mime_type TEXT NOT NULL DEFAULT 'image/png',
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                FOREIGN KEY (clip_uuid) REFERENCES clips(uuid) ON DELETE CASCADE
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE INDEX IF NOT EXISTS idx_clip_images_storage ON clip_images(storage_kind);
            "#,
        )
        .execute(&self.pool)
        .await?;

        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS clip_formats (
                clip_uuid TEXT NOT NULL,
                format TEXT NOT NULL,
                content BLOB NOT NULL,
                PRIMARY KEY (clip_uuid, format),
                FOREIGN KEY (clip_uuid) REFERENCES clips(uuid) ON DELETE CASCADE
            )
            "#,
        )
        .execute(&self.pool)
        .await?;

        // Older Cubby builds stored file clipboard payloads as external path
        // references. Those rows are not durable history: they can stop working
        // after files move, storage disconnects, or a target rejects CF_HDROP.
        // Remove them before the in-memory search index is built so no stale
        // file entry remains visible in All, folders, or search.
        let removed_file_references =
            sqlx::query("DELETE FROM clips WHERE clip_type IN ('file', 'files')")
                .execute(&self.pool)
                .await?
                .rows_affected();
        let mut notices = Vec::new();
        if removed_file_references > 0 {
            notices.push(to_startup_line(
                crate::startup_recovery_log::removed_file_references(removed_file_references),
            ));
        }

        Ok(notices)
    }
}

fn to_startup_line(notice: crate::startup_recovery_log::RecoveryNotice) -> (log::Level, String) {
    let level = match notice.level {
        crate::startup_recovery_log::RecoveryLevel::Error => log::Level::Error,
        crate::startup_recovery_log::RecoveryLevel::Warn => log::Level::Warn,
        crate::startup_recovery_log::RecoveryLevel::Info => log::Level::Info,
    };
    (level, notice.message)
}

/// Ensure `db_path` is either absent (will be created) or a healthy SQLite file.
///
/// Quarantine runs only when an integrity check itself reports corruption, or
/// when SQLite proves the file is not a database. Transient open/query errors
/// (BUSY, LOCKED, I/O, permissions) leave the original file untouched and
/// surface a startup error instead (SBS-770).
///
/// History-rewrite notices are returned so `run_app` can flush them through
/// `startup_log` after the logger exists (SBS-929).
async fn prepare_database_file(db_path: &Path) -> Result<Vec<(log::Level, String)>, String> {
    if !db_path.exists() {
        return Ok(Vec::new());
    }

    apply_database_health(db_path, assess_database_file(db_path).await).await
}

async fn apply_database_health(
    db_path: &Path,
    health: DatabaseHealth,
) -> Result<Vec<(log::Level, String)>, String> {
    match health {
        DatabaseHealth::Healthy => {
            if let Err(error) = refresh_rolling_backup(db_path).await {
                // Backup is best-effort; a full disk must not block capture.
                return Ok(vec![to_startup_line(
                    crate::startup_recovery_log::backup_refresh_failed(&error),
                )]);
            }
            Ok(Vec::new())
        }
        DatabaseHealth::Corrupt { reason } => {
            // Structural diagnostics only: never log row contents.
            let sanitized = sanitize_storage_diagnostic(&reason);
            let path = db_path.to_path_buf();
            tokio::task::spawn_blocking(move || quarantine_database_files(&path))
                .await
                .map_err(|e| format!("quarantine task failed: {e}"))??;
            let restore = restore_from_rolling_backup(db_path).await;
            Ok(
                crate::startup_recovery_log::notices_for_corrupt(&sanitized, restore)
                    .into_iter()
                    .map(to_startup_line)
                    .collect(),
            )
        }
        DatabaseHealth::Unassessable { reason } => Err(format!(
            "could not assess clipboard history ({}); the existing database was left untouched",
            sanitize_storage_diagnostic(&reason)
        )),
    }
}

/// After a corrupt database is quarantined, bring back the rolling backup so
/// the user keeps up to 24h-old history instead of silently starting from
/// zero. The backup file itself is kept (copy, not rename) as a second chance
/// for manual recovery. Best-effort: any failure falls back to a fresh file.
async fn restore_from_rolling_backup(
    db_path: &Path,
) -> crate::startup_recovery_log::RestoreOutcome {
    let backup = rolling_backup_path(db_path);
    if !backup.exists() {
        return crate::startup_recovery_log::RestoreOutcome::NoBackup;
    }

    match assess_database_file(&backup).await {
        DatabaseHealth::Healthy => {}
        DatabaseHealth::Corrupt { reason } | DatabaseHealth::Unassessable { reason } => {
            return crate::startup_recovery_log::RestoreOutcome::BackupUnusable {
                reason: sanitize_storage_diagnostic(&reason),
            };
        }
    }

    let source = backup.clone();
    let destination = db_path.to_path_buf();
    match tokio::task::spawn_blocking(move || std::fs::copy(&source, &destination)).await {
        Ok(Ok(_)) => crate::startup_recovery_log::RestoreOutcome::Restored {
            path: backup.display().to_string(),
        },
        Ok(Err(error)) => crate::startup_recovery_log::RestoreOutcome::CopyFailed {
            error: error.to_string(),
        },
        Err(error) => crate::startup_recovery_log::RestoreOutcome::TaskFailed {
            error: error.to_string(),
        },
    }
}

/// How many times a recoverable health-check error is retried before startup
/// fails without touching the file.
const HEALTH_CHECK_ATTEMPTS: u32 = 5;
const HEALTH_CHECK_BUSY_TIMEOUT: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, PartialEq, Eq)]
enum DatabaseHealth {
    Healthy,
    /// Integrity check ran and reported damage, or SQLite proved this is not a
    /// database. The only case that may quarantine.
    Corrupt {
        reason: String,
    },
    /// Operational error: lock, I/O, permissions. Do not quarantine.
    Unassessable {
        reason: String,
    },
}

async fn assess_database_file(db_path: &Path) -> DatabaseHealth {
    let mut last_unassessable = None;
    for attempt in 0..HEALTH_CHECK_ATTEMPTS {
        match assess_database_file_once(db_path).await {
            DatabaseHealth::Healthy => return DatabaseHealth::Healthy,
            DatabaseHealth::Corrupt { reason } => {
                return DatabaseHealth::Corrupt { reason };
            }
            DatabaseHealth::Unassessable { reason } => {
                last_unassessable = Some(reason);
                if attempt + 1 < HEALTH_CHECK_ATTEMPTS {
                    let delay_ms = 20u64 << attempt.min(4);
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                }
            }
        }
    }
    DatabaseHealth::Unassessable {
        reason: last_unassessable.unwrap_or_else(|| "could not assess database".to_string()),
    }
}

async fn assess_database_file_once(db_path: &Path) -> DatabaseHealth {
    use sqlx::Connection;

    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(false)
        .foreign_keys(true)
        .busy_timeout(HEALTH_CHECK_BUSY_TIMEOUT);

    // One connection (not a pool) so Windows releases the file handle before
    // quarantine rename runs.
    let mut conn = match sqlx::SqliteConnection::connect_with(&options).await {
        Ok(conn) => conn,
        Err(error) => return classify_health_check_error(&error, "open"),
    };

    // quick_check catches most corruption at startup without a full page walk.
    let result: Result<String, sqlx::Error> = sqlx::query_scalar("PRAGMA quick_check")
        .fetch_one(&mut conn)
        .await;

    // Checkpoint so a subsequent file copy of the main DB is consistent.
    // A checkpoint error is not an integrity verdict: the file may still be
    // healthy, and failing here must not quarantine or block startup.
    let _ = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(&mut conn)
        .await;

    // Always close before returning so quarantine can rename on Windows.
    // Close failure is not an integrity verdict: never quarantine a healthy DB
    // just because the check connection failed to close cleanly.
    if let Err(error) = conn.close().await {
        log::warn!("STORAGE: Failed to close integrity-check connection: {error}");
    }

    match result {
        Ok(text) if text.eq_ignore_ascii_case("ok") => DatabaseHealth::Healthy,
        Ok(text) => DatabaseHealth::Corrupt {
            reason: format!("quick_check: {text}"),
        },
        Err(error) => classify_health_check_error(&error, "quick_check"),
    }
}

/// Kept for tests that only need "usable or not".
#[cfg(test)]
async fn verify_database_quick_check(db_path: &Path) -> Result<(), String> {
    match assess_database_file(db_path).await {
        DatabaseHealth::Healthy => Ok(()),
        DatabaseHealth::Corrupt { reason } | DatabaseHealth::Unassessable { reason } => Err(reason),
    }
}

fn classify_health_check_error(error: &sqlx::Error, stage: &str) -> DatabaseHealth {
    match error {
        sqlx::Error::Database(database_error) => classify_sqlite_health_failure(
            sqlx::error::DatabaseError::code(database_error.as_ref()).as_deref(),
            &format!("{stage} failed: {database_error}"),
        ),
        sqlx::Error::Io(io_error) => DatabaseHealth::Unassessable {
            reason: format!("{stage} failed: {io_error}"),
        },
        sqlx::Error::PoolTimedOut => DatabaseHealth::Unassessable {
            reason: format!("{stage} failed: timed out"),
        },
        other => classify_sqlite_health_failure(None, &format!("{stage} failed: {other}")),
    }
}

fn classify_sqlite_health_failure(code: Option<&str>, reason: &str) -> DatabaseHealth {
    if sqlite_failure_is_corruption(code, reason) {
        DatabaseHealth::Corrupt {
            reason: reason.to_string(),
        }
    } else {
        DatabaseHealth::Unassessable {
            reason: reason.to_string(),
        }
    }
}

fn sqlite_failure_is_corruption(code: Option<&str>, reason: &str) -> bool {
    if let Some(primary) = sqlite_primary_result_code(code) {
        // SQLITE_CORRUPT = 11, SQLITE_NOTADB = 26.
        if primary == 11 || primary == 26 {
            return true;
        }
        // Operational primary codes are never treated as corruption.
        if matches!(primary, 3 | 5 | 6 | 8 | 10 | 13 | 14 | 15) {
            return false;
        }
    }

    let lower = reason.to_ascii_lowercase();
    lower.contains("not a database")
        || lower.contains("file is encrypted or is not a database")
        || lower.contains("malformed")
}

fn sqlite_primary_result_code(code: Option<&str>) -> Option<i64> {
    let code = code?;
    let parsed = code
        .strip_prefix("SQLITE_")
        .unwrap_or(code)
        .parse::<i64>()
        .ok()?;
    Some(parsed & 0xff)
}

type BackupInstaller = fn(&Path, &Path) -> Result<(), std::io::Error>;

/// What a refresh attempt actually did. A refusal is a completed pass, not an
/// installed copy: `cubby.db.bak` keeps both its bytes and its mtime, so the
/// caller must not read the on-disk gate as "this pass moved the copy
/// forward" and must not re-enter the checkpoint on the next tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackupRefreshOutcome {
    /// A new copy was checkpointed, written, and installed.
    Installed,
    /// The live history is far smaller than the backup; the copy was refused.
    RefusedNearEmptyDatabase,
}

async fn refresh_rolling_backup(db_path: &Path) -> Result<(), String> {
    let backup = rolling_backup_path(db_path);
    if backup_is_fresh(&backup) {
        return Ok(());
    }
    perform_rolling_backup_refresh(db_path).await.map(|_| ())
}

async fn perform_rolling_backup_refresh(db_path: &Path) -> Result<BackupRefreshOutcome, String> {
    perform_rolling_backup_refresh_with(db_path, replace_backup_atomically).await
}

async fn perform_rolling_backup_refresh_with(
    db_path: &Path,
    install: BackupInstaller,
) -> Result<BackupRefreshOutcome, String> {
    use sqlx::Connection;

    let backup = rolling_backup_path(db_path);

    // A rich backup must never be clobbered by a drastically smaller database
    // (fresh file after a corruption event, wiped history). Unknown counts
    // (missing tables on a brand-new file) fall through to a normal refresh.
    // This decision comes before the checkpoint on purpose:
    // `PRAGMA wal_checkpoint(TRUNCATE)` is the disruptive half of a refresh
    // and a refused copy has no use for it.
    if backup.exists() {
        if let (Some(current), Some(existing)) = (
            count_clips_in_file(db_path).await,
            count_clips_in_file(&backup).await,
        ) {
            if should_skip_backup_refresh(current, existing) {
                log::warn!(
                    "STORAGE: Keeping existing history backup ({existing} clips); current database only has {current}"
                );
                return Ok(BackupRefreshOutcome::RefusedNearEmptyDatabase);
            }
        }
    }

    // Always checkpoint here so the backup path is safe to invoke alone in tests.
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(false);
    let mut conn = sqlx::SqliteConnection::connect_with(&options)
        .await
        .map_err(|e| format!("backup open failed: {e}"))?;
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(&mut conn)
        .await
        .map_err(|e| format!("backup checkpoint failed: {e}"))?;
    if let Err(error) = conn.close().await {
        log::warn!("STORAGE: Failed to close backup checkpoint connection: {error}");
    }

    let temporary = db_path.with_file_name(format!(
        "{}.bak.{}.{}.tmp",
        file_stem_lossy(db_path),
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let source = db_path.to_path_buf();
    let temporary_for_copy = temporary.clone();
    let taken_at = SystemTime::now();
    let copied = tokio::task::spawn_blocking(move || {
        copy_backup_snapshot(&source, &temporary_for_copy, taken_at)
    })
    .await;
    match copied {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return Err(format!("backup copy failed: {error}")),
        Err(error) => {
            // A panicking copy task leaves whatever it had written.
            let _ = std::fs::remove_file(&temporary);
            return Err(format!("backup copy task failed: {error}"));
        }
    }

    let temporary_for_rename = temporary.clone();
    let backup_for_rename = backup.clone();
    let installed = tokio::task::spawn_blocking(move || {
        install(&temporary_for_rename, &backup_for_rename).inspect_err(|_| {
            let _ = std::fs::remove_file(&temporary_for_rename);
        })
    })
    .await;
    match installed {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return Err(format!("backup install failed: {error}")),
        Err(error) => {
            let _ = std::fs::remove_file(&temporary);
            return Err(format!("backup install task failed: {error}"));
        }
    }

    log::info!(
        "STORAGE: Refreshed rolling history backup at {}",
        backup.display()
    );
    Ok(BackupRefreshOutcome::Installed)
}

/// Copy the checkpointed history file to `temporary` and stamp it with when
/// the copy ran.
///
/// A failed copy cleans up after itself. `std::fs::copy` creates (or
/// truncates) the destination before it writes, so a failure part-way through
/// — the disk filling up is the realistic one — leaves a partial
/// `*.bak.*.tmp` behind. Nothing else would ever remove it, and the refresh
/// runs hourly, so a persistent I/O failure would pile those up beside the
/// user's history file.
fn copy_backup_snapshot(
    source: &Path,
    temporary: &Path,
    taken_at: SystemTime,
) -> Result<(), std::io::Error> {
    if let Err(error) = std::fs::copy(source, temporary) {
        let _ = std::fs::remove_file(temporary);
        return Err(error);
    }
    if let Err(error) = stamp_backup_taken_at(temporary, taken_at) {
        // Bytes matter more than the timestamp: an unstamped copy is a valid
        // recovery point, it just looks older than it is.
        log::warn!("STORAGE: Could not stamp history backup time: {error}");
    }
    Ok(())
}

/// Record when the copy was taken. On Windows `std::fs::copy` is
/// `CopyFileEx`, which carries the *source* file's last-write time onto the
/// destination, and a `TRUNCATE` checkpoint over an already-empty WAL does not
/// move that time. An idle machine would therefore write a brand-new
/// `cubby.db.bak` that is born older than the max age, and every later
/// freshness check would order another checkpoint-plus-copy. The mtime is what
/// the gate reads, so it has to mean "when the backup ran".
fn stamp_backup_taken_at(path: &Path, taken_at: SystemTime) -> Result<(), std::io::Error> {
    std::fs::File::options()
        .write(true)
        .open(path)?
        .set_modified(taken_at)
}

#[cfg(target_os = "windows")]
fn replace_backup_atomically(source: &Path, destination: &Path) -> Result<(), std::io::Error> {
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
    .map_err(std::io::Error::other)
}

#[cfg(not(target_os = "windows"))]
fn replace_backup_atomically(source: &Path, destination: &Path) -> Result<(), std::io::Error> {
    std::fs::rename(source, destination)
}

/// Refuse a backup refresh when it would replace a substantial backup with a
/// near-empty database. The 10% threshold tolerates normal retention shrinkage
/// while catching the reset-to-zero cases that matter.
fn should_skip_backup_refresh(current_clips: i64, backup_clips: i64) -> bool {
    backup_clips >= 100 && current_clips.saturating_mul(10) < backup_clips
}

async fn count_clips_in_file(path: &Path) -> Option<i64> {
    use sqlx::Connection;

    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(false)
        .read_only(true);
    let mut conn = sqlx::SqliteConnection::connect_with(&options).await.ok()?;
    let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM clips WHERE is_deleted = 0")
        .fetch_one(&mut conn)
        .await
        .ok();
    if let Err(error) = conn.close().await {
        log::warn!("STORAGE: Failed to close clip-count connection: {error}");
    }
    count
}

fn backup_is_fresh(backup: &Path) -> bool {
    backup_is_fresh_as_of(backup, SystemTime::now(), ROLLING_BACKUP_MAX_AGE)
}

/// Missing file, unreadable metadata, and unreadable mtime are all "not
/// fresh". Do not collapse those into "nothing to do" — the next tick should
/// try to write a copy.
fn backup_is_fresh_as_of(backup: &Path, now: SystemTime, max_age: Duration) -> bool {
    let Ok(metadata) = std::fs::metadata(backup) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    age_at(now, modified) < max_age
}

/// How old `stamp` is at `now`, with a future stamp clamped to zero. A clock
/// that jumps forward and back, or a backup carried over from a machine whose
/// clock ran ahead, leaves an mtime later than `now`. Reading that as "no age
/// available, so not fresh" would order a checkpoint and a full file copy on
/// every tick until the clock catches up.
fn age_at(now: SystemTime, stamp: SystemTime) -> Duration {
    now.duration_since(stamp).unwrap_or(Duration::ZERO)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RollingBackupTick {
    /// A current recovery point already exists; no checkpoint, no copy.
    Fresh,
    /// Checkpoint + atomic replace ran.
    Refreshed,
    /// The live history was too small to overwrite the backup with. The `.bak`
    /// keeps both its bytes and its mtime, so the next tick recounts and
    /// decides again — cheaply, because the count runs before the checkpoint.
    /// That re-check is the point: a history rebuilt past the skip rule must
    /// not wait out the max-age window before it can be backed up.
    Refused,
    /// A refresh is already in flight.
    Busy,
    /// Copy/install failed; the previous `.bak` is untouched.
    Failed(String),
}

/// Releases the in-flight flag even if a refresh unwinds, so a panic cannot
/// permanently disable in-session backups.
struct RollingBackupInFlightGuard<'a> {
    flag: &'a AtomicBool,
}

impl Drop for RollingBackupInFlightGuard<'_> {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::SeqCst);
    }
}

struct RollingBackupScheduler {
    db_path: PathBuf,
    max_age: Duration,
    in_flight: AtomicBool,
    /// Distinct from "backup is fresh": a failed attempt must retry on the
    /// next tick even if a last-attempt timestamp would look current.
    last_refresh_failed: AtomicBool,
}

impl RollingBackupScheduler {
    fn new(db_path: PathBuf) -> Self {
        Self::with_max_age(db_path, ROLLING_BACKUP_MAX_AGE)
    }

    fn with_max_age(db_path: PathBuf, max_age: Duration) -> Self {
        Self {
            db_path,
            max_age,
            in_flight: AtomicBool::new(false),
            last_refresh_failed: AtomicBool::new(false),
        }
    }

    fn try_begin(&self) -> Option<RollingBackupInFlightGuard<'_>> {
        self.in_flight
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| RollingBackupInFlightGuard {
                flag: &self.in_flight,
            })
    }

    async fn tick(&self, now: SystemTime) -> RollingBackupTick {
        self.tick_using(now, replace_backup_atomically).await
    }

    async fn tick_using(&self, now: SystemTime, installer: BackupInstaller) -> RollingBackupTick {
        let Some(_guard) = self.try_begin() else {
            return RollingBackupTick::Busy;
        };

        let last_failed = self.last_refresh_failed.load(Ordering::SeqCst);
        // The `.bak` file's own mtime is the only record of freshness, and it
        // is deliberately the only one. A session-local "last pass" stamp would
        // also cover the refused passes that leave that mtime alone, but it
        // would then vouch for a backup that is stale, or deleted, or that a
        // rebuilt history should now be allowed to replace — for the whole
        // max-age window, on a session that never quits.
        //
        // Re-entering a refused pass costs two clip counts and nothing else:
        // the count check runs before `PRAGMA wal_checkpoint(TRUNCATE)`, so it
        // never stalls capture.
        let backup = rolling_backup_path(&self.db_path);
        if !last_failed && backup_is_fresh_as_of(&backup, now, self.max_age) {
            return RollingBackupTick::Fresh;
        }

        match perform_rolling_backup_refresh_with(&self.db_path, installer).await {
            Ok(outcome) => {
                self.last_refresh_failed.store(false, Ordering::SeqCst);
                match outcome {
                    BackupRefreshOutcome::Installed => RollingBackupTick::Refreshed,
                    BackupRefreshOutcome::RefusedNearEmptyDatabase => RollingBackupTick::Refused,
                }
            }
            Err(error) => {
                self.last_refresh_failed.store(true, Ordering::SeqCst);
                RollingBackupTick::Failed(error)
            }
        }
    }
}

/// One supervised background task: after storage is ready, periodically ask
/// whether `cubby.db.bak` is stale and refresh it without blocking capture
/// or the UI. Startup already took one pass via `apply_database_health`.
pub(crate) fn start_rolling_backup_scheduler(db_path: PathBuf) {
    if ROLLING_BACKUP_SCHEDULER_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }

    log::info!(
        "STORAGE: Scheduling rolling history backup checks every {}s",
        ROLLING_BACKUP_CHECK_INTERVAL.as_secs()
    );

    tauri::async_runtime::spawn(async move {
        let scheduler = RollingBackupScheduler::new(db_path);
        loop {
            // Tick first so a failed startup refresh retries without waiting
            // a full hour. A successful startup pass leaves the backup fresh,
            // so this is a metadata check, not a second copy.
            match scheduler.tick(SystemTime::now()).await {
                RollingBackupTick::Fresh
                | RollingBackupTick::Refreshed
                | RollingBackupTick::Refused
                | RollingBackupTick::Busy => {}
                RollingBackupTick::Failed(error) => {
                    log::warn!("STORAGE: Could not refresh history backup: {error}");
                }
            }
            tokio::time::sleep(ROLLING_BACKUP_CHECK_INTERVAL).await;
        }
    });
}

fn rolling_backup_path(db_path: &Path) -> PathBuf {
    // `cubby.db` → `cubby.db.bak` (keep the original name readable).
    let name = db_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "cubby.db".to_string());
    db_path.with_file_name(format!("{name}.bak"))
}

fn sqlite_sidecar_path(db_path: &Path, suffix: &str) -> PathBuf {
    // Append `-wal` / `-shm` to the full path bytes (not via Display, which is lossy).
    let mut name = db_path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn quarantine_database_files(db_path: &Path) -> Result<(), String> {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let parent = db_path.parent().unwrap_or_else(|| Path::new("."));

    // SQLite names WAL/SHM as `{main}-wal` / `{main}-shm` (suffix on full name).
    let candidates = [
        db_path.to_path_buf(),
        sqlite_sidecar_path(db_path, "-wal"),
        sqlite_sidecar_path(db_path, "-shm"),
    ];

    let mut moved_any = false;
    for source in candidates {
        if !source.exists() {
            continue;
        }
        let file_name = source
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "cubby.db".to_string());
        let destination = parent.join(format!("{file_name}.corrupt-{stamp}"));
        rename_with_share_retry(&source, &destination).map_err(|e| {
            format!(
                "failed to quarantine unusable history file {}: {e}",
                source.display()
            )
        })?;
        log::warn!(
            "STORAGE: Quarantined unusable history file to {}",
            destination.display()
        );
        moved_any = true;
    }

    if !moved_any {
        return Err(
            "clipboard history database is unusable, but no files could be quarantined".to_string(),
        );
    }
    Ok(())
}

fn file_stem_lossy(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "cubby".to_string())
}

/// Windows can keep a SQLite handle briefly after close/failed open (ERROR_SHARING_VIOLATION).
fn rename_with_share_retry(source: &Path, destination: &Path) -> Result<(), String> {
    const ATTEMPTS: u32 = 12;
    let mut last_error = None;
    for attempt in 0..ATTEMPTS {
        match std::fs::rename(source, destination) {
            Ok(()) => return Ok(()),
            Err(error) => {
                let sharing = error.raw_os_error() == Some(32)
                    || error.kind() == std::io::ErrorKind::PermissionDenied;
                last_error = Some(error);
                if !sharing || attempt + 1 == ATTEMPTS {
                    break;
                }
                std::thread::sleep(Duration::from_millis(25 * u64::from(attempt + 1)));
            }
        }
    }
    Err(last_error
        .map(|e| e.to_string())
        .unwrap_or_else(|| "rename failed".to_string()))
}

/// Strip/truncate integrity diagnostics so logs never become a dump of page
/// payloads. SQLite messages are structural, but we still bound them.
fn sanitize_storage_diagnostic(message: &str) -> String {
    const MAX: usize = 180;
    let flat: String = message
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let flat = flat.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= MAX {
        flat
    } else {
        let truncated: String = flat.chars().take(MAX).collect();
        format!("{truncated}...")
    }
}

/// One clip competing to survive deduplication.
struct DuplicateRow {
    uuid: String,
    is_pinned: bool,
    is_hidden: bool,
    folder_id: Option<i64>,
    notes: Option<String>,
}

async fn add_column_if_missing(pool: &SqlitePool, sql: &str) -> Result<(), sqlx::Error> {
    match sqlx::query(sql).execute(pool).await {
        Ok(_) => Ok(()),
        Err(e) => {
            let msg = e.to_string().to_lowercase();
            if msg.contains("duplicate column name") {
                Ok(())
            } else {
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_database_health, backup_is_fresh_as_of, classify_sqlite_health_failure,
        copy_backup_snapshot, count_clips_in_file, perform_rolling_backup_refresh,
        prepare_database_file, quarantine_database_files, replace_backup_atomically,
        rolling_backup_path, sanitize_storage_diagnostic, should_skip_backup_refresh,
        sqlite_failure_is_corruption, verify_database_quick_check, Database, DatabaseHealth,
        RollingBackupScheduler, RollingBackupTick,
    };
    use crate::crypto::CryptoManager;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime};

    fn temp_dir() -> std::path::PathBuf {
        let directory =
            std::env::temp_dir().join(format!("cubby-db-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    /// SBS-929: a history rewrite that only `log::`'d would return no lines.
    fn assert_has_notice(notices: &[(log::Level, String)], level: log::Level, needle: &str) {
        assert!(
            notices
                .iter()
                .any(|(got_level, message)| *got_level == level && message.contains(needle)),
            "expected {level} notice containing {needle:?}, got {notices:?}"
        );
    }

    /// uuid, is_pinned, is_hidden, folder_id, notes.
    type SurvivorState = (String, bool, bool, Option<i64>, Option<String>);

    async fn migrated_database() -> Database {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory database should open");
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&pool)
            .await
            .expect("foreign keys should be enabled");
        let database = Database {
            pool,
            crypto: Arc::new(CryptoManager::ephemeral()),
            image_dir: std::env::temp_dir().join(format!("cubby-test-{}", uuid::Uuid::new_v4())),
            search_index: Arc::new(crate::search_index::SearchIndex::default()),
        };
        database.migrate().await.expect("migration should succeed");
        database
    }

    async fn insert_clip_with_hash(
        database: &Database,
        uuid: &str,
        hash: &str,
        pinned: bool,
        folder: Option<i64>,
        created_at: &str,
    ) {
        sqlx::query(
            r#"INSERT INTO clips (uuid, clip_type, content, text_preview, content_hash, folder_id, is_pinned, created_at, last_accessed)
               VALUES (?, 'text', x'00', 'preview', ?, ?, ?, ?, ?)"#,
        )
        .bind(uuid)
        .bind(hash)
        .bind(folder)
        .bind(pinned)
        .bind(created_at)
        .bind(created_at)
        .execute(&database.pool)
        .await
        .expect("clip should insert");
    }

    async fn unique_hash_index_exists(database: &Database) -> bool {
        sqlx::query_scalar::<_, bool>(
            r#"
            SELECT EXISTS(
                SELECT 1 FROM sqlite_master
                WHERE type = 'index' AND name = 'idx_clips_hash_unique'
            )
            "#,
        )
        .fetch_one(&database.pool)
        .await
        .expect("index lookup should succeed")
    }

    #[tokio::test]
    async fn a_fresh_database_gets_the_unique_hash_index() {
        let database = migrated_database().await;
        assert_eq!(
            database
                .enforce_content_hash_uniqueness()
                .await
                .expect("a fresh database has nothing to reconcile"),
            0
        );
        assert!(unique_hash_index_exists(&database).await);

        // The old non-unique index is gone, so nothing suggests the constraint
        // is merely advisory.
        let old_index: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = 'idx_clips_hash')",
        )
        .fetch_one(&database.pool)
        .await
        .expect("index lookup should succeed");
        assert!(!old_index, "the old non-unique index should be replaced");
    }

    #[tokio::test]
    async fn migrate_does_not_rebuild_the_old_hash_index_after_uniqueness() {
        let database = migrated_database().await;
        database
            .enforce_content_hash_uniqueness()
            .await
            .expect("a fresh database has nothing to reconcile");
        database
            .migrate()
            .await
            .expect("a second migrate on an already-unique database must succeed");
        let old_index: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = 'idx_clips_hash')",
        )
        .fetch_one(&database.pool)
        .await
        .expect("index lookup should succeed");
        assert!(
            !old_index,
            "startup must not rebuild the non-unique index uniqueness just dropped"
        );
        assert!(unique_hash_index_exists(&database).await);
    }

    #[tokio::test]
    async fn a_database_without_duplicates_upgrades_cleanly() {
        let database = migrated_database().await;
        for index in 0..5 {
            insert_clip_with_hash(
                &database,
                &format!("uuid{index}"),
                &format!("hash{index}"),
                false,
                None,
                "2026-05-01 09:00:00",
            )
            .await;
        }

        assert_eq!(
            database.enforce_content_hash_uniqueness().await.unwrap(),
            0,
            "nothing should be removed when there are no duplicates"
        );
        let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM clips")
            .fetch_one(&database.pool)
            .await
            .unwrap();
        assert_eq!(remaining, 5, "no clip may be lost");
    }

    #[tokio::test]
    async fn duplicates_collapse_to_the_oldest_visible_row_and_keep_hand_set_state() {
        let database = migrated_database().await;
        let folder: i64 =
            sqlx::query_scalar("INSERT INTO folders (name) VALUES ('Kept') RETURNING id")
                .fetch_one(&database.pool)
                .await
                .expect("folder should insert");

        // Every piece of hand-set state is on the NEWER row, which is the one
        // that loses. All of it has to land on the survivor, or the user
        // silently loses work they cannot recover.
        insert_clip_with_hash(
            &database,
            "original",
            "same-hash",
            false,
            None,
            "2026-05-01 09:00:00",
        )
        .await;
        insert_clip_with_hash(
            &database,
            "duplicate",
            "same-hash",
            true,
            Some(folder),
            "2026-05-02 09:00:00",
        )
        .await;
        sqlx::query(
            "UPDATE clips SET notes = 'invoice reference', is_hidden = 1 WHERE uuid = 'duplicate'",
        )
        .execute(&database.pool)
        .await
        .expect("note should save");

        assert_eq!(database.enforce_content_hash_uniqueness().await.unwrap(), 1);

        let survivors: Vec<SurvivorState> =
            sqlx::query_as("SELECT uuid, is_pinned, is_hidden, folder_id, notes FROM clips")
                .fetch_all(&database.pool)
                .await
                .unwrap();
        assert_eq!(survivors.len(), 1, "one row should remain");
        assert_eq!(
            survivors[0].0, "original",
            "the oldest visible row should win"
        );
        assert!(survivors[0].1, "the pin should carry forward");
        assert!(survivors[0].2, "hide state should carry forward");
        assert_eq!(survivors[0].3, Some(folder), "folder should carry forward");
        assert_eq!(
            survivors[0].4.as_deref(),
            Some("invoice reference"),
            "the user's note should carry forward"
        );
        assert!(unique_hash_index_exists(&database).await);
    }

    /// Soft delete keeps the row, so a newer deleted duplicate must never win:
    /// hard-deleting the only visible copy would make the clip vanish from
    /// history entirely. `remove_duplicate_clips` documents the same hazard.
    #[tokio::test]
    async fn a_soft_deleted_duplicate_never_outranks_the_visible_clip() {
        let database = migrated_database().await;
        insert_clip_with_hash(
            &database,
            "visible",
            "hash",
            false,
            None,
            "2026-05-01 09:00:00",
        )
        .await;
        insert_clip_with_hash(
            &database,
            "deleted",
            "hash",
            false,
            None,
            "2026-05-09 09:00:00",
        )
        .await;
        sqlx::query("UPDATE clips SET is_deleted = 1 WHERE uuid = 'deleted'")
            .execute(&database.pool)
            .await
            .expect("soft delete should apply");

        assert_eq!(database.enforce_content_hash_uniqueness().await.unwrap(), 1);

        let remaining: Vec<(String, bool)> = sqlx::query_as("SELECT uuid, is_deleted FROM clips")
            .fetch_all(&database.pool)
            .await
            .unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].0, "visible", "the visible clip must survive");
        assert!(!remaining[0].1, "the survivor must still be visible");
    }

    /// A pin on a row that is about to be removed has to move to the survivor,
    /// even when the survivor is the visible-but-older copy.
    #[tokio::test]
    async fn a_pin_on_a_soft_deleted_duplicate_moves_to_the_visible_survivor() {
        let database = migrated_database().await;
        insert_clip_with_hash(
            &database,
            "visible",
            "hash",
            false,
            None,
            "2026-05-01 09:00:00",
        )
        .await;
        insert_clip_with_hash(
            &database,
            "deleted",
            "hash",
            true,
            None,
            "2026-05-09 09:00:00",
        )
        .await;
        sqlx::query("UPDATE clips SET is_deleted = 1 WHERE uuid = 'deleted'")
            .execute(&database.pool)
            .await
            .expect("soft delete should apply");

        database.enforce_content_hash_uniqueness().await.unwrap();

        let (uuid, pinned, deleted): (String, bool, bool) =
            sqlx::query_as("SELECT uuid, is_pinned, is_deleted FROM clips")
                .fetch_one(&database.pool)
                .await
                .unwrap();
        assert_eq!(uuid, "visible");
        assert!(pinned, "the pin should survive on the visible row");
        assert!(!deleted, "a live pinned clip must remain live");
    }

    #[tokio::test]
    async fn a_duplicate_hash_is_rejected_after_the_constraint_exists() {
        let database = migrated_database().await;
        insert_clip_with_hash(
            &database,
            "first",
            "hash",
            false,
            None,
            "2026-05-01 09:00:00",
        )
        .await;
        database.enforce_content_hash_uniqueness().await.unwrap();

        // The point of the whole change: the database refuses, so a caller that
        // mistakes a failed lookup for a miss cannot duplicate history.
        let result = sqlx::query(
            r#"INSERT INTO clips (uuid, clip_type, content, text_preview, content_hash)
               VALUES ('second', 'text', x'00', 'preview', 'hash')"#,
        )
        .execute(&database.pool)
        .await;

        let error = result.expect_err("a duplicate content_hash must be rejected");
        assert!(
            error.to_string().to_lowercase().contains("unique"),
            "expected a uniqueness violation, got: {error}"
        );
    }

    #[tokio::test]
    async fn child_rows_of_a_removed_duplicate_go_with_it() {
        let database = migrated_database().await;
        insert_clip_with_hash(&database, "old", "dupe", false, None, "2026-05-01 09:00:00").await;
        insert_clip_with_hash(&database, "new", "dupe", false, None, "2026-05-02 09:00:00").await;
        // Attached to the row that loses -- the newer one, now that the oldest
        // visible copy survives.
        sqlx::query(
            "INSERT INTO clip_formats (clip_uuid, format, content) VALUES ('new', 'fixture', x'00')",
        )
        .execute(&database.pool)
        .await
        .expect("format row should insert");

        database.enforce_content_hash_uniqueness().await.unwrap();

        let orphans: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM clip_formats")
            .fetch_one(&database.pool)
            .await
            .unwrap();
        assert_eq!(orphans, 0, "child rows must not outlive their clip");
    }

    #[tokio::test]
    async fn migration_adds_pin_state_to_existing_clip_tables() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory database should open");

        sqlx::query(
            r#"
            CREATE TABLE clips (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                uuid TEXT NOT NULL UNIQUE,
                clip_type TEXT NOT NULL,
                content BLOB NOT NULL,
                text_preview TEXT,
                content_hash TEXT NOT NULL,
                folder_id INTEGER,
                is_deleted INTEGER DEFAULT 0,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                last_accessed DATETIME DEFAULT CURRENT_TIMESTAMP
            )
            "#,
        )
        .execute(&pool)
        .await
        .expect("legacy clips table should be created");

        let database = Database {
            pool,
            crypto: Arc::new(CryptoManager::ephemeral()),
            image_dir: std::env::temp_dir().join(format!("cubby-test-{}", uuid::Uuid::new_v4())),
            search_index: Arc::new(crate::search_index::SearchIndex::default()),
        };
        database.migrate().await.expect("migration should succeed");

        let pin_default: i64 = sqlx::query_scalar(
            r#"
            SELECT CAST("dflt_value" AS INTEGER)
            FROM pragma_table_info('clips')
            WHERE name = 'is_pinned'
            "#,
        )
        .fetch_one(&database.pool)
        .await
        .expect("is_pinned column should exist");

        assert_eq!(pin_default, 0);
    }

    #[tokio::test]
    async fn migration_removes_legacy_file_references_and_their_formats() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("in-memory database should open");
        sqlx::query("PRAGMA foreign_keys = ON")
            .execute(&pool)
            .await
            .expect("foreign keys should enable");
        let database = Database {
            pool,
            crypto: Arc::new(CryptoManager::ephemeral()),
            image_dir: std::env::temp_dir().join(format!("cubby-test-{}", uuid::Uuid::new_v4())),
            search_index: Arc::new(crate::search_index::SearchIndex::default()),
        };
        database
            .migrate()
            .await
            .expect("initial migration should succeed");

        for (uuid, clip_type) in [
            ("legacy-file", "file"),
            ("legacy-files", "files"),
            ("keep-text", "text"),
            ("keep-image", "image"),
        ] {
            sqlx::query(
                r#"
                INSERT INTO clips (uuid, clip_type, content, text_preview, content_hash)
                VALUES (?, ?, x'00', '', ?)
                "#,
            )
            .bind(uuid)
            .bind(clip_type)
            .bind(format!("hash-{uuid}"))
            .execute(&database.pool)
            .await
            .expect("legacy fixture should insert");
        }
        for uuid in ["legacy-file", "legacy-files", "keep-text"] {
            sqlx::query(
                "INSERT INTO clip_formats (clip_uuid, format, content) VALUES (?, 'fixture', x'00')",
            )
            .bind(uuid)
            .execute(&database.pool)
            .await
            .expect("format fixture should insert");
        }

        database
            .migrate()
            .await
            .expect("upgrade migration should succeed");

        let remaining: Vec<String> = sqlx::query_scalar("SELECT uuid FROM clips ORDER BY uuid")
            .fetch_all(&database.pool)
            .await
            .expect("remaining clips should query");
        assert_eq!(remaining, vec!["keep-image", "keep-text"]);

        let remaining_formats: Vec<String> =
            sqlx::query_scalar("SELECT clip_uuid FROM clip_formats ORDER BY clip_uuid")
                .fetch_all(&database.pool)
                .await
                .expect("remaining formats should query");
        assert_eq!(remaining_formats, vec!["keep-text"]);
    }

    #[tokio::test]
    async fn encrypted_database_without_its_key_fails_closed() {
        let directory =
            std::env::temp_dir().join(format!("cubby-db-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let database_path = directory.join("cubby.db");
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&database_path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO settings (key, value) VALUES ('storage_encryption_version', '1')")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;

        let error = Database::new(database_path.to_str().unwrap())
            .await
            .err()
            .expect("missing protected key should stop startup");
        assert!(error.to_string().contains("storage key is missing"));
        assert!(!directory.join("storage.key").exists());
        // SQLx may release the failed constructor's SQLite handle just after the
        // error is returned on Windows, so cleanup is deliberately best effort.
        let _ = std::fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn missing_database_file_is_ready_for_create() {
        let directory = temp_dir();
        let database_path = directory.join("cubby.db");
        prepare_database_file(&database_path)
            .await
            .expect("missing file should not need recovery");
        assert!(!database_path.exists());
        let _ = std::fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn healthy_database_gets_a_rolling_backup() {
        let directory = temp_dir();
        let database_path = directory.join("cubby.db");
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&database_path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE t (id INTEGER PRIMARY KEY)")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;

        prepare_database_file(&database_path)
            .await
            .expect("healthy database should pass recovery");
        assert!(database_path.exists());
        assert!(
            rolling_backup_path(&database_path).exists(),
            "first healthy open should write cubby.db.bak"
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    async fn write_history_marker(path: &std::path::Path, marker: &str) {
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE IF NOT EXISTS t (id INTEGER PRIMARY KEY, name TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM t").execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO t (name) VALUES (?)")
            .bind(marker)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    async fn read_history_marker(path: &std::path::Path) -> String {
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(false);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        let name: String = sqlx::query_scalar("SELECT name FROM t")
            .fetch_one(&pool)
            .await
            .unwrap();
        pool.close().await;
        name
    }

    fn fail_backup_install(_: &std::path::Path, _: &std::path::Path) -> Result<(), std::io::Error> {
        Err(std::io::Error::other("simulated install failure"))
    }

    fn modified_time(path: &std::path::Path) -> SystemTime {
        std::fs::metadata(path).unwrap().modified().unwrap()
    }

    fn set_modified_time(path: &std::path::Path, at: SystemTime) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(at)
            .unwrap();
    }

    /// Seed a file that `count_clips_in_file` can read, with `clips` live rows.
    async fn write_clips_file(path: &std::path::Path, clips: i64) {
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE IF NOT EXISTS clips (id INTEGER PRIMARY KEY, is_deleted INTEGER NOT NULL DEFAULT 0)")
            .execute(&pool)
            .await
            .unwrap();
        for _ in 0..clips {
            sqlx::query("INSERT INTO clips (is_deleted) VALUES (0)")
                .execute(&pool)
                .await
                .unwrap();
        }
        sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }

    /// Windows `std::fs::copy` is `CopyFileEx`, which stamps the copy with the
    /// source file's last-write time, and a `TRUNCATE` checkpoint over an
    /// empty WAL leaves that time alone. Without an explicit stamp the backup
    /// of an idle history file is born older than the max age, so the very
    /// next tick checkpoints and copies again, and so does every tick after
    /// that (SBS-771).
    #[tokio::test]
    async fn a_backup_of_an_idle_database_is_stamped_when_it_was_taken() {
        let directory = temp_dir();
        let database_path = directory.join("cubby.db");
        write_history_marker(&database_path, "idle").await;

        // The overnight case this targets: history untouched for two days.
        let two_days_ago = SystemTime::now() - Duration::from_secs(48 * 60 * 60);
        set_modified_time(&database_path, two_days_ago);

        let taken_at = SystemTime::now();
        perform_rolling_backup_refresh(&database_path)
            .await
            .expect("refresh should install a copy");

        let backup = rolling_backup_path(&database_path);
        let backup_modified = modified_time(&backup);
        assert!(
            backup_modified + Duration::from_secs(60) >= taken_at,
            "backup mtime must record when the copy ran, not how old the history file is"
        );

        let before = std::fs::read(&backup).unwrap();
        let scheduler =
            RollingBackupScheduler::with_max_age(database_path.clone(), Duration::from_secs(60));
        let outcome = scheduler
            .tick(backup_modified + Duration::from_secs(1))
            .await;
        assert_eq!(
            outcome,
            RollingBackupTick::Fresh,
            "the next tick must not recopy a backup that was taken moments ago"
        );
        assert_eq!(
            std::fs::read(&backup).unwrap(),
            before,
            "a fresh backup must keep its bytes"
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    /// A near-empty history must not clobber a rich backup, and the refusal
    /// must be cheap enough to repeat: the clip counts run before
    /// `PRAGMA wal_checkpoint(TRUNCATE)`, so a repeated refusal never touches
    /// the live database (SBS-771).
    #[tokio::test]
    async fn a_refused_refresh_never_checkpoints_the_live_database() {
        let directory = temp_dir();
        let database_path = directory.join("cubby.db");
        let backup = rolling_backup_path(&database_path);
        write_clips_file(&backup, 1000).await;
        write_clips_file(&database_path, 0).await;

        let backup_before = std::fs::read(&backup).unwrap();
        let backup_modified = modified_time(&backup);
        let database_modified = modified_time(&database_path);

        let max_age = Duration::from_secs(60);
        let scheduler = RollingBackupScheduler::with_max_age(database_path.clone(), max_age);
        let stale = backup_modified + max_age + Duration::from_secs(1);

        for label in ["the first refusal", "the next hourly tick"] {
            assert_eq!(
                scheduler.tick(stale).await,
                RollingBackupTick::Refused,
                "a wiped history must not overwrite a 1000-clip backup ({label})"
            );
        }
        assert_eq!(
            std::fs::read(&backup).unwrap(),
            backup_before,
            "the rich backup must keep its bytes"
        );
        assert_eq!(
            modified_time(&database_path),
            database_modified,
            "a refused refresh must not checkpoint the live history file"
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    /// A refusal must not vouch for the backup. Once the user rebuilds enough
    /// history to clear the skip rule, the very next tick has to back it up --
    /// not wait out the max-age window because an earlier pass "completed"
    /// (SBS-771).
    #[tokio::test]
    async fn a_rebuilt_history_is_backed_up_on_the_tick_after_a_refusal() {
        let directory = temp_dir();
        let database_path = directory.join("cubby.db");
        let backup = rolling_backup_path(&database_path);
        write_clips_file(&backup, 1000).await;
        write_clips_file(&database_path, 0).await;

        let max_age = Duration::from_secs(60);
        let scheduler = RollingBackupScheduler::with_max_age(database_path.clone(), max_age);
        let stale = modified_time(&backup) + max_age + Duration::from_secs(1);

        assert_eq!(scheduler.tick(stale).await, RollingBackupTick::Refused);

        // 200 live clips against a 1000-clip backup clears the skip rule
        // (200 * 10 is not less than 1000).
        write_clips_file(&database_path, 200).await;
        assert_eq!(
            scheduler.tick(stale + Duration::from_secs(1)).await,
            RollingBackupTick::Refreshed,
            "a history rebuilt past the skip rule must be backed up now, not in 24h"
        );
        assert_eq!(
            count_clips_in_file(&backup).await,
            Some(200),
            "the installed backup must hold the rebuilt history"
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    /// `std::fs::copy` creates the destination before it writes, so a failure
    /// part-way through (a full disk) leaves a partial `*.bak.*.tmp`. Nothing
    /// else removes it and the refresh runs hourly, so a persistent I/O
    /// failure would pile them up beside the history file (SBS-771).
    #[test]
    fn a_failed_backup_copy_removes_the_partial_temporary_file() {
        let directory = temp_dir();
        let temporary = directory.join("cubby.bak.1234.tmp");
        // Stand in for what a part-way copy leaves on disk.
        std::fs::write(&temporary, b"half a database").unwrap();

        let error = copy_backup_snapshot(
            &directory.join("no-such-history.db"),
            &temporary,
            SystemTime::now(),
        )
        .expect_err("copying a missing source must fail");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(
            !temporary.exists(),
            "a failed copy must not leave partial backup bytes behind"
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn leftover_image_temps_are_removed_on_open() {
        let directory = temp_dir();
        let stale = directory.join("abc.cubby.tmp");
        let keep = directory.join("abc.cubby");
        std::fs::write(&stale, b"orphaned staging bytes").unwrap();
        std::fs::write(&keep, b"live original").unwrap();
        Database::sweep_stale_image_temps(&directory);
        assert!(!stale.exists(), "a crash-left staging file must not survive");
        assert!(keep.exists(), "committed originals must be left alone");
        let _ = std::fs::remove_dir_all(directory);
    }

    /// The `.bak` file is the recovery point. If OneDrive, a cleanup tool, or
    /// the user deletes it, the next tick has to write a new one -- a session
    /// that never quits must not be left with no rolling copy at all (SBS-771).
    #[tokio::test]
    async fn a_deleted_backup_is_recreated_on_the_next_tick() {
        let directory = temp_dir();
        let database_path = directory.join("cubby.db");
        write_clips_file(&database_path, 5).await;
        let backup = rolling_backup_path(&database_path);

        let max_age = Duration::from_secs(60);
        let scheduler = RollingBackupScheduler::with_max_age(database_path.clone(), max_age);
        assert_eq!(
            scheduler.tick(SystemTime::now()).await,
            RollingBackupTick::Refreshed
        );
        assert!(backup.exists());

        std::fs::remove_file(&backup).expect("the backup should be removable");
        assert_eq!(
            scheduler.tick(SystemTime::now()).await,
            RollingBackupTick::Refreshed,
            "a missing backup must be recreated rather than reported fresh"
        );
        assert!(
            backup.exists(),
            "there must be a rolling recovery copy on disk again"
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    /// A clock that jumps forward and back, or a backup carried over from a
    /// machine running ahead, leaves an mtime later than now. Reading that as
    /// "no age available, so stale" costs a checkpoint and a full file copy on
    /// every tick until the clock catches up (SBS-771).
    #[test]
    fn a_backup_stamped_in_the_future_is_treated_as_fresh() {
        let directory = temp_dir();
        let backup = directory.join("cubby.db.bak");
        std::fs::write(&backup, b"copied from a machine running ahead").unwrap();

        let modified = modified_time(&backup);
        let now = modified - Duration::from_secs(60 * 60);
        assert!(
            backup_is_fresh_as_of(&backup, now, Duration::from_secs(60)),
            "a backup newer than the clock must count as fresh, not force a recopy"
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    /// A session that stays up past the backup cadence must refresh
    /// `cubby.db.bak` without another process start. Startup-only refresh
    /// fails this: the in-session tick never copies, so backup bytes and
    /// mtime stay put (SBS-771).
    #[tokio::test]
    async fn a_session_longer_than_one_backup_interval_refreshes_the_rolling_backup() {
        let directory = temp_dir();
        let database_path = directory.join("cubby.db");
        write_history_marker(&database_path, "v1").await;
        perform_rolling_backup_refresh(&database_path)
            .await
            .expect("seed backup should write");

        let backup = rolling_backup_path(&database_path);
        assert_eq!(read_history_marker(&backup).await, "v1");
        let before_modified = std::fs::metadata(&backup).unwrap().modified().unwrap();

        write_history_marker(&database_path, "v2").await;
        let max_age = Duration::from_secs(60);
        let now = before_modified + max_age + Duration::from_secs(1);
        let scheduler = RollingBackupScheduler::with_max_age(database_path.clone(), max_age);

        let outcome = scheduler.tick(now).await;
        assert_eq!(
            outcome,
            RollingBackupTick::Refreshed,
            "crossing the cadence must refresh, not wait for another launch"
        );
        assert_eq!(
            read_history_marker(&backup).await,
            "v2",
            "the recovery copy must pick up history written after the last startup"
        );
        let after_modified = std::fs::metadata(&backup).unwrap().modified().unwrap();
        assert!(
            after_modified > before_modified,
            "backup mtime must move when the in-session refresh installs a new copy"
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    /// A failed in-session refresh must keep the last known-good `.bak` and
    /// retry on the next tick. Collapsing "refresh failed" into "backup is
    /// fresh" would wait another cadence or a restart (SBS-771).
    #[tokio::test]
    async fn a_failed_refresh_keeps_the_known_good_backup_and_retries() {
        let directory = temp_dir();
        let database_path = directory.join("cubby.db");
        write_history_marker(&database_path, "known-good").await;
        perform_rolling_backup_refresh(&database_path)
            .await
            .expect("seed backup should write");

        let backup = rolling_backup_path(&database_path);
        let known_good = std::fs::read(&backup).unwrap();
        let backup_modified = std::fs::metadata(&backup).unwrap().modified().unwrap();

        write_history_marker(&database_path, "newer").await;
        let max_age = Duration::from_secs(60);
        let stale_now = backup_modified + max_age + Duration::from_secs(1);
        let scheduler = RollingBackupScheduler::with_max_age(database_path.clone(), max_age);

        let failed = scheduler.tick_using(stale_now, fail_backup_install).await;
        assert!(
            matches!(failed, RollingBackupTick::Failed(_)),
            "install failure must surface as failed, not as a fresh skip, got {failed:?}"
        );
        assert_eq!(
            std::fs::read(&backup).unwrap(),
            known_good,
            "a failed install must leave the last known-good backup bytes"
        );

        // Within max-age of the original mtime. A tick that treated the
        // failed attempt as "fresh" would skip; last_refresh_failed must
        // force a retry instead.
        let would_look_fresh = backup_modified + Duration::from_secs(1);
        let retried = scheduler.tick(would_look_fresh).await;
        assert_eq!(
            retried,
            RollingBackupTick::Refreshed,
            "the next tick must retry after a failed refresh"
        );
        assert_eq!(read_history_marker(&backup).await, "newer");
        let _ = std::fs::remove_dir_all(directory);
    }

    /// A second tick must not start another checkpoint or copy while one is
    /// already in flight. Overlapping WAL checkpoints can stall capture and
    /// two copies can race on the same temp/rename (SBS-771).
    #[tokio::test]
    async fn a_second_tick_does_not_start_an_overlapping_refresh() {
        let scheduler = RollingBackupScheduler::with_max_age(
            std::path::PathBuf::from("unused.db"),
            Duration::from_secs(60),
        );
        let _hold = scheduler.try_begin().expect("first claim should succeed");
        let outcome = scheduler.tick(SystemTime::now()).await;
        assert_eq!(
            outcome,
            RollingBackupTick::Busy,
            "an in-flight refresh must reject a second tick"
        );
    }

    /// An in-session tick must still respect the max-age gate. Rewriting
    /// every hour would checkpoint and copy the live history file constantly.
    #[tokio::test]
    async fn an_in_session_tick_does_not_rewrite_a_fresh_backup() {
        let directory = temp_dir();
        let database_path = directory.join("cubby.db");
        write_history_marker(&database_path, "v1").await;
        perform_rolling_backup_refresh(&database_path)
            .await
            .expect("seed backup should write");

        let backup = rolling_backup_path(&database_path);
        write_history_marker(&database_path, "v2").await;
        let before = std::fs::read(&backup).unwrap();
        let before_modified = std::fs::metadata(&backup).unwrap().modified().unwrap();

        let scheduler =
            RollingBackupScheduler::with_max_age(database_path.clone(), Duration::from_secs(60));
        let outcome = scheduler
            .tick(before_modified + Duration::from_secs(1))
            .await;
        assert_eq!(outcome, RollingBackupTick::Fresh);
        assert_eq!(std::fs::read(&backup).unwrap(), before);
        assert_eq!(
            std::fs::metadata(&backup).unwrap().modified().unwrap(),
            before_modified
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn a_missing_or_unreadable_backup_is_not_treated_as_fresh() {
        let missing =
            std::env::temp_dir().join(format!("cubby-missing-backup-{}.bak", uuid::Uuid::new_v4()));
        assert!(
            !backup_is_fresh_as_of(&missing, SystemTime::now(), Duration::from_secs(60)),
            "unknown mtime must not count as a fresh recovery copy"
        );
    }

    #[tokio::test]
    async fn fresh_rolling_backup_is_not_rewritten() {
        let directory = temp_dir();
        let database_path = directory.join("cubby.db");
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&database_path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE t (id INTEGER PRIMARY KEY)")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;

        let backup = rolling_backup_path(&database_path);
        let marker = b"preexisting-fresh-backup";
        std::fs::write(&backup, marker).unwrap();
        let before_meta = std::fs::metadata(&backup).unwrap();
        let before_modified = before_meta.modified().unwrap();

        prepare_database_file(&database_path)
            .await
            .expect("healthy database with fresh backup should pass");

        assert_eq!(std::fs::read(&backup).unwrap(), marker);
        let after_modified = std::fs::metadata(&backup).unwrap().modified().unwrap();
        assert_eq!(
            before_modified, after_modified,
            "backup younger than 24h must not be rewritten"
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn rolling_backup_refresh_replaces_an_existing_file() {
        let directory = temp_dir();
        let temporary = directory.join("cubby.db.bak.tmp");
        let backup = directory.join("cubby.db.bak");
        std::fs::write(&temporary, b"new backup").unwrap();
        std::fs::write(&backup, b"old backup").unwrap();

        replace_backup_atomically(&temporary, &backup)
            .expect("an expired Windows backup should be replaceable");

        assert!(!temporary.exists());
        assert_eq!(std::fs::read(&backup).unwrap(), b"new backup");
        let _ = std::fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn garbage_database_is_quarantined_and_fresh_open_succeeds() {
        let directory = temp_dir();
        let database_path = directory.join("cubby.db");
        let wal_path = std::path::PathBuf::from(format!("{}-wal", database_path.display()));
        std::fs::write(&database_path, b"this is not a sqlite database").unwrap();
        std::fs::write(&wal_path, b"wal-garbage").unwrap();

        let notices = prepare_database_file(&database_path)
            .await
            .expect("corrupt history should be quarantined");
        assert_has_notice(&notices, log::Level::Error, "quarantining");
        assert_has_notice(&notices, log::Level::Warn, "empty history");

        assert!(!database_path.exists(), "main DB should be moved aside");
        assert!(!wal_path.exists(), "WAL sidecar should be moved aside");

        let quarantined: Vec<_> = std::fs::read_dir(&directory)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains("corrupt-"))
            .collect();
        assert!(
            quarantined
                .iter()
                .any(|name| name.starts_with("cubby.db.corrupt-")),
            "expected quarantined main file, got {quarantined:?}"
        );
        // WAL is quarantined when still present; SQLite may also drop a junk
        // `-wal` during a failed open, which is fine as long as it is gone.
        assert!(
            !wal_path.exists()
                || quarantined
                    .iter()
                    .any(|name| name.starts_with("cubby.db-wal.corrupt-")),
            "WAL should be gone or quarantined, got {quarantined:?}"
        );

        // Fresh open after quarantine must create a usable database.
        let (db, _notices) = Database::new(database_path.to_str().unwrap())
            .await
            .expect("fresh database should open after quarantine");
        db.migrate().await.expect("fresh schema should apply");
        db.pool.close().await;
        let _ = std::fs::remove_dir_all(directory);
    }

    #[tokio::test]
    async fn corrupt_database_is_restored_from_rolling_backup() {
        let directory = temp_dir();
        let database_path = directory.join("cubby.db");
        let backup_path = rolling_backup_path(&database_path);

        // Healthy backup with a recognizable row.
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&backup_path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO t (name) VALUES ('survivor')")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;

        std::fs::write(&database_path, b"this is not a sqlite database").unwrap();

        let notices = prepare_database_file(&database_path)
            .await
            .expect("corrupt history should be quarantined and restored");
        assert_has_notice(&notices, log::Level::Error, "quarantining");
        assert_has_notice(
            &notices,
            log::Level::Warn,
            "Restored clipboard history from rolling backup",
        );
        assert!(
            !notices
                .iter()
                .any(|(_, message)| message.contains("empty history")),
            "a successful restore must not also claim empty history, got {notices:?}"
        );

        assert!(
            database_path.exists(),
            "database should be restored from the backup"
        );
        assert!(backup_path.exists(), "backup must survive the restore");
        verify_database_quick_check(&database_path)
            .await
            .expect("restored database should be healthy");

        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&database_path)
            .create_if_missing(false);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        let name: String = sqlx::query_scalar("SELECT name FROM t")
            .fetch_one(&pool)
            .await
            .unwrap();
        pool.close().await;
        assert_eq!(name, "survivor");
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn backup_refresh_skips_when_current_is_drastically_smaller() {
        // Reset-to-zero cases keep the rich backup.
        assert!(should_skip_backup_refresh(0, 100));
        assert!(should_skip_backup_refresh(5, 1000));
        // Normal retention shrinkage still refreshes.
        assert!(!should_skip_backup_refresh(50, 100));
        assert!(!should_skip_backup_refresh(500, 1000));
        // Small histories never block a refresh.
        assert!(!should_skip_backup_refresh(0, 99));
    }

    #[tokio::test]
    async fn structurally_corrupt_sqlite_is_quarantined() {
        let directory = temp_dir();
        let database_path = directory.join("cubby.db");
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(&database_path)
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE t (id INTEGER PRIMARY KEY, payload BLOB)")
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("INSERT INTO t (payload) VALUES (?)")
            .bind(vec![7_u8; 4096])
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;

        // Punch a hole in the file body while keeping the SQLite header so open
        // may succeed but quick_check should fail.
        let mut bytes = std::fs::read(&database_path).unwrap();
        assert!(
            bytes.len() > 200,
            "fixture should be larger than the header"
        );
        for byte in bytes.iter_mut().skip(100).take(80) {
            *byte = 0xFF;
        }
        std::fs::write(&database_path, bytes).unwrap();

        let notices = prepare_database_file(&database_path)
            .await
            .expect("corrupt sqlite should quarantine rather than fail startup");
        assert_has_notice(&notices, log::Level::Error, "quarantining");
        assert!(!database_path.exists());
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn storage_diagnostics_are_bounded_and_flattened() {
        let noisy = format!("page {}\x07\n{}", "x".repeat(400), "y".repeat(400));
        let clean = sanitize_storage_diagnostic(&noisy);
        assert!(!clean.contains('\n'));
        assert!(!clean.contains('\u{7}'));
        assert!(clean.chars().count() <= 183);
    }

    #[test]
    fn busy_locked_and_io_errors_are_not_corruption() {
        for (code, message) in [
            (Some("5"), "open failed: database is locked"),
            (Some("6"), "quick_check failed: database table is locked"),
            (Some("10"), "open failed: disk I/O error"),
            (Some("14"), "open failed: unable to open database file"),
            (Some("261"), "open failed: database is locked"),
            (None, "open failed: permission denied"),
        ] {
            assert!(
                !sqlite_failure_is_corruption(code, message),
                "{code:?} {message} must not quarantine"
            );
            assert!(matches!(
                classify_sqlite_health_failure(code, message),
                DatabaseHealth::Unassessable { .. }
            ));
        }
    }

    #[test]
    fn integrity_and_notadb_failures_are_corruption() {
        assert!(matches!(
            classify_sqlite_health_failure(Some("26"), "open failed: file is not a database"),
            DatabaseHealth::Corrupt { .. }
        ));
        assert!(matches!(
            classify_sqlite_health_failure(Some("11"), "quick_check failed: malformed"),
            DatabaseHealth::Corrupt { .. }
        ));
        assert!(sqlite_failure_is_corruption(
            None,
            "open failed: file is not a database"
        ));
    }

    #[tokio::test]
    async fn a_transient_health_error_leaves_the_database_untouched() {
        let directory = temp_dir();
        let database_path = directory.join("cubby.db");
        let payload = b"keep-this-history-file";
        std::fs::write(&database_path, payload).unwrap();

        let result = apply_database_health(
            &database_path,
            DatabaseHealth::Unassessable {
                reason: "database is locked".to_string(),
            },
        )
        .await;

        assert!(
            result.is_err(),
            "a transient failure must surface a startup error, got {result:?}"
        );
        let error = result.unwrap_err();
        assert!(
            error.contains("left untouched"),
            "expected a non-destructive error, got: {error}"
        );
        assert!(database_path.exists(), "original path must remain");
        assert_eq!(std::fs::read(&database_path).unwrap(), payload);
        let quarantined: Vec<_> = std::fs::read_dir(&directory)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains("corrupt-"))
            .collect();
        assert!(
            quarantined.is_empty(),
            "transient errors must not quarantine, got {quarantined:?}"
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn quarantine_rename_preserves_sibling_key_file() {
        let directory = temp_dir();
        let database_path = directory.join("cubby.db");
        std::fs::write(&database_path, b"broken").unwrap();
        std::fs::write(directory.join("storage.key"), b"key-bytes").unwrap();
        quarantine_database_files(&database_path).unwrap();
        assert!(!database_path.exists());
        assert_eq!(
            std::fs::read(directory.join("storage.key")).unwrap(),
            b"key-bytes"
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    /// SBS-929: an unusable rolling backup is an empty-history fallback, and
    /// that line must be returned rather than only `log::`'d.
    #[tokio::test]
    async fn unusable_rolling_backup_reports_empty_history() {
        let directory = temp_dir();
        let database_path = directory.join("cubby.db");
        let backup_path = rolling_backup_path(&database_path);
        std::fs::write(&database_path, b"this is not a sqlite database").unwrap();
        std::fs::write(&backup_path, b"this is not a sqlite backup").unwrap();

        let notices = prepare_database_file(&database_path)
            .await
            .expect("corrupt history should fail open even when the backup is unusable");
        assert_has_notice(&notices, log::Level::Error, "quarantining");
        assert_has_notice(
            &notices,
            log::Level::Error,
            "Rolling backup failed verification",
        );
        assert_has_notice(&notices, log::Level::Error, "empty history");
        assert!(
            !database_path.exists(),
            "unusable backup must not be installed"
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    /// SBS-929: `apply_database_health` itself must return the lines. A
    /// `log::` inside this function is the discarded path the ticket names.
    #[tokio::test]
    async fn apply_database_health_returns_quarantine_notices() {
        let directory = temp_dir();
        let database_path = directory.join("cubby.db");
        std::fs::write(&database_path, b"broken").unwrap();

        let notices = apply_database_health(
            &database_path,
            DatabaseHealth::Corrupt {
                reason: "file is not a database".to_string(),
            },
        )
        .await
        .expect("corrupt health should fail open");
        assert_has_notice(&notices, log::Level::Error, "quarantining");
        assert_has_notice(&notices, log::Level::Warn, "empty history");
        assert!(
            !notices.is_empty(),
            "returning no notices is the pre-fix bug (log:: only)"
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    /// SBS-929: the healthy path still has one thing to say. A refresh that
    /// cannot write a copy is best-effort for capture, but the user must be
    /// able to find out that the recovery copy is not current.
    ///
    /// The fixture is a database file that is gone by the time the refresh
    /// opens it — the file-level failure a full disk or a revoked permission
    /// produces at the same point. `apply_database_health` is called with
    /// `Healthy` directly, which is the arm under test.
    #[tokio::test]
    async fn healthy_path_reports_a_failed_backup_refresh() {
        let directory = temp_dir();
        let database_path = directory.join("cubby.db");
        assert!(
            !database_path.exists(),
            "the refresh must be the thing that fails, not the health check"
        );

        let notices = apply_database_health(&database_path, DatabaseHealth::Healthy)
            .await
            .expect("a failed backup refresh must not block startup");
        assert_has_notice(
            &notices,
            log::Level::Warn,
            "Could not refresh history backup",
        );
        let _ = std::fs::remove_dir_all(directory);
    }

    /// SBS-929: `Database::new` must hand its caller the lines
    /// `prepare_database_file` produced. Dropping that vec on the `Ok` tuple
    /// is exactly the silent-startup bug, and every other test in this file
    /// calls `prepare_database_file` directly, so nothing else would catch it.
    #[tokio::test]
    async fn database_new_returns_the_prepare_notices() {
        let directory = temp_dir();
        let database_path = directory.join("cubby.db");
        std::fs::write(&database_path, b"this is not a sqlite database").unwrap();

        let (_database, notices) = Database::new(database_path.to_str().unwrap())
            .await
            .expect("a quarantined history should still open a fresh database");
        assert_has_notice(&notices, log::Level::Error, "quarantining");
        assert_has_notice(&notices, log::Level::Warn, "empty history");
        let _ = std::fs::remove_dir_all(directory);
    }

    /// SBS-929: the leftover migrate `log::info!` for file-reference rows
    /// must come back as a collectable notice.
    #[tokio::test]
    async fn migrate_reports_removed_legacy_file_references() {
        let database = migrated_database().await;
        sqlx::query(
            r#"INSERT INTO clips (uuid, clip_type, content, text_preview, content_hash)
               VALUES ('file-1', 'file', x'00', 'preview', 'file-hash')"#,
        )
        .execute(&database.pool)
        .await
        .expect("legacy file-reference row should insert");

        let notices = database
            .migrate()
            .await
            .expect("migrate should remove the leftover file row");
        assert_has_notice(
            &notices,
            log::Level::Info,
            "legacy file-reference history items",
        );
        let remaining: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM clips WHERE clip_type = 'file'")
                .fetch_one(&database.pool)
                .await
                .expect("count should query");
        assert_eq!(remaining, 0);
    }
}
