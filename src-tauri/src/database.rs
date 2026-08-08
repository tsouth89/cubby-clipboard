use sqlx::SqlitePool;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::crypto::CryptoManager;
use crate::search_index::SearchIndex;

/// How often a healthy on-disk history file is snapshotted to `cubby.db.bak`.
/// Startup stays cheap while still giving a recent recovery point after bad
/// migrations or abrupt power loss.
const ROLLING_BACKUP_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone)]
pub struct Database {
    pub pool: SqlitePool,
    pub crypto: Arc<CryptoManager>,
    pub image_dir: PathBuf,
    pub search_index: Arc<SearchIndex>,
}

impl Database {
    pub async fn new(db_path: &str) -> Result<Self, String> {
        let path = Path::new(db_path);
        // Fail open for capture: a corrupt history is quarantined and replaced
        // with an empty database rather than blocking the app (SOU-218).
        prepare_database_file(path).await?;

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

        Ok(Self {
            pool,
            crypto,
            image_dir,
            search_index: Arc::new(crate::search_index::SearchIndex::default()),
        })
    }

    pub async fn migrate(&self) -> Result<(), sqlx::Error> {
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
            CREATE INDEX IF NOT EXISTS idx_clips_hash ON clips(content_hash);
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
        if removed_file_references > 0 {
            log::info!(
                "STORAGE: Removed {} legacy file-reference history items",
                removed_file_references
            );
        }

        Ok(())
    }
}

/// Ensure `db_path` is either absent (will be created) or a healthy SQLite file.
///
/// On open/integrity failure the existing `cubby.db` (+ WAL/SHM) is renamed to a
/// timestamped quarantine name and startup continues with a fresh file. The
/// DPAPI-protected `storage.key` and image blobs are left alone so a future
/// manual recovery from the quarantine file can still decrypt.
async fn prepare_database_file(db_path: &Path) -> Result<(), String> {
    if !db_path.exists() {
        return Ok(());
    }

    match verify_database_quick_check(db_path).await {
        Ok(()) => {
            if let Err(error) = refresh_rolling_backup(db_path).await {
                // Backup is best-effort; a full disk must not block capture.
                log::warn!("STORAGE: Could not refresh history backup: {error}");
            }
            Ok(())
        }
        Err(reason) => {
            // Structural diagnostics only: never log row contents.
            log::error!(
                "STORAGE: Clipboard history database is unusable ({}); quarantining",
                sanitize_storage_diagnostic(&reason)
            );
            let path = db_path.to_path_buf();
            tokio::task::spawn_blocking(move || quarantine_database_files(&path))
                .await
                .map_err(|e| format!("quarantine task failed: {e}"))??;
            restore_from_rolling_backup(db_path).await;
            Ok(())
        }
    }
}

/// After a corrupt database is quarantined, bring back the rolling backup so
/// the user keeps up to 24h-old history instead of silently starting from
/// zero. The backup file itself is kept (copy, not rename) as a second chance
/// for manual recovery. Best-effort: any failure falls back to a fresh file.
async fn restore_from_rolling_backup(db_path: &Path) {
    let backup = rolling_backup_path(db_path);
    if !backup.exists() {
        log::warn!("STORAGE: No rolling backup found; starting with an empty history");
        return;
    }

    if let Err(error) = verify_database_quick_check(&backup).await {
        log::error!(
            "STORAGE: Rolling backup failed verification ({}); starting with an empty history",
            sanitize_storage_diagnostic(&error)
        );
        return;
    }

    let source = backup.clone();
    let destination = db_path.to_path_buf();
    match tokio::task::spawn_blocking(move || std::fs::copy(&source, &destination)).await {
        Ok(Ok(_)) => log::warn!(
            "STORAGE: Restored clipboard history from rolling backup {}",
            backup.display()
        ),
        Ok(Err(error)) => log::error!(
            "STORAGE: Could not restore history backup: {error}; starting with an empty history"
        ),
        Err(error) => log::error!(
            "STORAGE: Backup restore task failed: {error}; starting with an empty history"
        ),
    }
}

async fn verify_database_quick_check(db_path: &Path) -> Result<(), String> {
    use sqlx::Connection;

    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(false)
        .foreign_keys(true);

    // One connection (not a pool) so Windows releases the file handle before
    // quarantine rename runs.
    let mut conn = sqlx::SqliteConnection::connect_with(&options)
        .await
        .map_err(|e| format!("open failed: {e}"))?;

    // quick_check catches most corruption at startup without a full page walk.
    let result: Result<String, sqlx::Error> = sqlx::query_scalar("PRAGMA quick_check")
        .fetch_one(&mut conn)
        .await;

    // Checkpoint so a subsequent file copy of the main DB is consistent.
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
        Ok(text) if text.eq_ignore_ascii_case("ok") => Ok(()),
        Ok(text) => Err(format!("quick_check: {text}")),
        Err(e) => Err(format!("quick_check failed: {e}")),
    }
}

async fn refresh_rolling_backup(db_path: &Path) -> Result<(), String> {
    use sqlx::Connection;

    let backup = rolling_backup_path(db_path);
    if backup_is_fresh(&backup) {
        return Ok(());
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

    // A rich backup must never be clobbered by a drastically smaller database
    // (fresh file after a corruption event, wiped history). Unknown counts
    // (missing tables on a brand-new file) fall through to a normal refresh.
    if backup.exists() {
        if let (Some(current), Some(existing)) = (
            count_clips_in_file(db_path).await,
            count_clips_in_file(&backup).await,
        ) {
            if should_skip_backup_refresh(current, existing) {
                log::warn!(
                    "STORAGE: Keeping existing history backup ({existing} clips); current database only has {current}"
                );
                return Ok(());
            }
        }
    }

    let temporary = db_path.with_file_name(format!(
        "{}.bak.{}.{}.tmp",
        file_stem_lossy(db_path),
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let source = db_path.to_path_buf();
    let temporary_for_copy = temporary.clone();
    tokio::task::spawn_blocking(move || std::fs::copy(&source, &temporary_for_copy))
        .await
        .map_err(|e| format!("backup copy task failed: {e}"))?
        .map_err(|e| format!("backup copy failed: {e}"))?;

    let temporary_for_rename = temporary.clone();
    let backup_for_rename = backup.clone();
    tokio::task::spawn_blocking(move || {
        replace_backup_atomically(&temporary_for_rename, &backup_for_rename).inspect_err(|_| {
            let _ = std::fs::remove_file(&temporary_for_rename);
        })
    })
    .await
    .map_err(|e| format!("backup install task failed: {e}"))?
    .map_err(|e| format!("backup install failed: {e}"))?;

    log::info!(
        "STORAGE: Refreshed rolling history backup at {}",
        backup.display()
    );
    Ok(())
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
    let Ok(metadata) = std::fs::metadata(backup) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    modified
        .elapsed()
        .map(|age| age < ROLLING_BACKUP_MAX_AGE)
        .unwrap_or(false)
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
        prepare_database_file, quarantine_database_files, replace_backup_atomically,
        rolling_backup_path, sanitize_storage_diagnostic, should_skip_backup_refresh,
        verify_database_quick_check, Database,
    };
    use crate::crypto::CryptoManager;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::sync::Arc;

    fn temp_dir() -> std::path::PathBuf {
        let directory =
            std::env::temp_dir().join(format!("cubby-db-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        directory
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
        assert!(error.contains("storage key is missing"));
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

        prepare_database_file(&database_path)
            .await
            .expect("corrupt history should be quarantined");

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
        let db = Database::new(database_path.to_str().unwrap())
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

        prepare_database_file(&database_path)
            .await
            .expect("corrupt history should be quarantined and restored");

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

        prepare_database_file(&database_path)
            .await
            .expect("corrupt sqlite should quarantine rather than fail startup");
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
}
