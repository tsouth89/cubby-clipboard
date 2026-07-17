use sqlx::SqlitePool;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::crypto::CryptoManager;

#[derive(Clone)]
pub struct Database {
    pub pool: SqlitePool,
    pub crypto: Arc<CryptoManager>,
    pub image_dir: PathBuf,
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
