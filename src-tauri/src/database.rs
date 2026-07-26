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
                "STORAGE: Clipboard history database is unusable ({}); quarantining and starting fresh",
                sanitize_storage_diagnostic(&reason)
            );
            quarantine_database_files(db_path)?;
            Ok(())
        }
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
    conn.close()
        .await
        .map_err(|e| format!("close failed: {e}"))?;

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
    conn.close()
        .await
        .map_err(|e| format!("backup close failed: {e}"))?;

    let temporary = db_path.with_file_name(format!(
        "{}.bak.{}.{}.tmp",
        file_stem_lossy(db_path),
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::copy(db_path, &temporary).map_err(|e| format!("backup copy failed: {e}"))?;
    std::fs::rename(&temporary, &backup).map_err(|e| {
        let _ = std::fs::remove_file(&temporary);
        format!("backup install failed: {e}")
    })?;
    log::info!(
        "STORAGE: Refreshed rolling history backup at {}",
        backup.display()
    );
    Ok(())
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

fn quarantine_database_files(db_path: &Path) -> Result<(), String> {
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let parent = db_path.parent().unwrap_or_else(|| Path::new("."));

    // SQLite names WAL/SHM as `{main}-wal` / `{main}-shm` (suffix on full name).
    let candidates = [
        db_path.to_path_buf(),
        PathBuf::from(format!("{}-wal", db_path.display())),
        PathBuf::from(format!("{}-shm", db_path.display())),
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
        format!("{truncated}…")
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
        prepare_database_file, quarantine_database_files, rolling_backup_path,
        sanitize_storage_diagnostic, Database,
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
        assert!(bytes.len() > 200, "fixture should be larger than the header");
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
        assert!(clean.chars().count() <= 181);
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
