use crate::crypto::CryptoManager;
use crate::search_index::SearchIndex;
use sqlx::SqlitePool;
use std::path::PathBuf;
use std::sync::Arc;

pub struct Database {
    pub pool: SqlitePool,
    pub crypto: Arc<CryptoManager>,
    pub image_dir: PathBuf,
    pub search_index: Arc<SearchIndex>,
}

impl Database {
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
                is_hidden INTEGER NOT NULL DEFAULT 0,
                is_thumbnail INTEGER NOT NULL DEFAULT 0,
                source_app TEXT,
                source_icon TEXT,
                metadata TEXT,
                notes TEXT,
                ocr_text TEXT,
                ocr_status TEXT,
                full_image_expired INTEGER NOT NULL DEFAULT 0,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                last_accessed DATETIME DEFAULT CURRENT_TIMESTAMP
            )
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

        Ok(())
    }
}
