use sqlx::SqlitePool;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::crypto::CryptoManager;
use crate::search_index::SearchIndex;

#[derive(Clone)]
pub struct Database {
    pub pool: SqlitePool,
    pub crypto: Arc<CryptoManager>,
    pub image_dir: PathBuf,
    pub search_index: Arc<SearchIndex>,
}

impl Database {
    pub async fn new(db_path: &str) -> Result<Self, String> {
        let image_dir = Path::new(db_path)
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("images");
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(db_path)
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
            Path::new(db_path),
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
    use super::Database;
    use crate::crypto::CryptoManager;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::sync::Arc;

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
}
