//! Import clipboard history and pinned items from a Ditto database.
//!
//! Ditto stores clips in SQLite: `Main` holds one row per clip (`lID`, `lDate`
//! as Unix seconds, `mText`, `lDontAutoDelete` = keep/pinned, `bIsGroup`), and
//! `Data` holds the clipboard formats per clip (`lParentID` -> `Main.lID`,
//! `strClipBoardFormat`, `ooData` blob).
//!
//! v1 imports text clips (the bulk of a history) with their original dates and
//! pinned state, encrypting each one through Cubby's `CryptoManager` exactly as
//! native capture does. Image clips are counted and skipped for now.

use crate::database::Database;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{Row, SqlitePool};
use uuid::Uuid;

const PREVIEW_LIMIT: usize = 200;

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct DittoImportResult {
    pub total: usize,
    pub imported: usize,
    pub duplicates: usize,
    pub skipped_groups: usize,
    pub skipped_images: usize,
    pub skipped_empty: usize,
    pub errors: Vec<String>,
    pub dry_run: bool,
}

fn is_image_format(format: &str) -> bool {
    let f = format.to_ascii_uppercase();
    f == "PNG" || f.starts_with("CF_DIB") || f == "CF_BITMAP" || f.contains("IMAGE")
}

/// Ditto's `CF_UNICODETEXT` blob is UTF-16LE, usually NUL-terminated.
fn decode_utf16le(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 2 {
        return None;
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    let text = String::from_utf16_lossy(&units);
    let trimmed = text.trim_end_matches('\0');
    (!trimmed.trim().is_empty()).then(|| trimmed.to_string())
}

/// Best-effort decode of `CF_TEXT`/`CF_OEMTEXT` (single-byte), NUL-terminated.
fn decode_ansi(bytes: &[u8]) -> Option<String> {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    if end == 0 {
        return None;
    }
    let text = match std::str::from_utf8(&bytes[..end]) {
        Ok(valid) => valid.to_string(),
        Err(_) => bytes[..end].iter().map(|&b| b as char).collect(),
    };
    (!text.trim().is_empty()).then_some(text)
}

/// Pick the best text representation from a clip's formats, preferring unicode.
fn extract_text(formats: &[(String, Vec<u8>)]) -> Option<String> {
    let find = |name: &str| {
        formats
            .iter()
            .find(|(f, _)| f.eq_ignore_ascii_case(name))
            .map(|(_, d)| d)
    };
    if let Some(data) = find("CF_UNICODETEXT") {
        if let Some(text) = decode_utf16le(data) {
            return Some(text);
        }
    }
    for name in ["CF_TEXT", "CF_OEMTEXT"] {
        if let Some(data) = find(name) {
            if let Some(text) = decode_ansi(data) {
                return Some(text);
            }
        }
    }
    None
}

fn truncate_preview(text: &str) -> String {
    let mut preview = String::new();
    for ch in text.chars() {
        if preview.chars().count() >= PREVIEW_LIMIT {
            break;
        }
        preview.push(ch);
    }
    preview
}

/// Convert Ditto's Unix-seconds `lDate` to Cubby's `YYYY-MM-DD HH:MM:SS` UTC
/// text, matching the format SQLite's `CURRENT_TIMESTAMP` produces.
fn unix_to_datetime(seconds: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(seconds, 0)
        .unwrap_or_else(chrono::Utc::now)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

pub async fn import_from_ditto(
    db: &Database,
    ditto_db_path: &str,
    dry_run: bool,
) -> Result<DittoImportResult, String> {
    let options = SqliteConnectOptions::new()
        .filename(ditto_db_path)
        .read_only(true)
        .immutable(true);
    let ditto = SqlitePool::connect_with(options)
        .await
        .map_err(|e| format!("Could not open the Ditto database: {e}"))?;

    let clips = sqlx::query(
        "SELECT lID, lDate, mText, lDontAutoDelete, bIsGroup FROM Main ORDER BY lDate ASC",
    )
    .fetch_all(&ditto)
    .await
    .map_err(|e| format!("Could not read Ditto clips: {e}"))?;

    let mut result = DittoImportResult {
        dry_run,
        ..Default::default()
    };

    for row in clips {
        result.total += 1;
        let lid: i64 = row.try_get("lID").unwrap_or(0);
        let ldate: i64 = row.try_get("lDate").unwrap_or(0);
        let mtext: Option<String> = row.try_get("mText").ok().flatten();
        let dont_delete: i64 = row.try_get("lDontAutoDelete").unwrap_or(0);
        let is_group: i64 = row.try_get("bIsGroup").unwrap_or(0);

        if is_group != 0 {
            result.skipped_groups += 1;
            continue;
        }

        let data_rows =
            sqlx::query("SELECT strClipBoardFormat, ooData FROM Data WHERE lParentID = ?")
                .bind(lid)
                .fetch_all(&ditto)
                .await
                .map_err(|e| format!("Could not read data for Ditto clip {lid}: {e}"))?;

        let formats: Vec<(String, Vec<u8>)> = data_rows
            .iter()
            .map(|r| {
                (
                    r.try_get::<String, _>("strClipBoardFormat")
                        .unwrap_or_default(),
                    r.try_get::<Vec<u8>, _>("ooData").unwrap_or_default(),
                )
            })
            .collect();

        let text = extract_text(&formats).or_else(|| mtext.filter(|t| !t.trim().is_empty()));

        let text = match text {
            Some(text) => text,
            None => {
                if formats.iter().any(|(f, _)| is_image_format(f)) {
                    result.skipped_images += 1;
                } else {
                    result.skipped_empty += 1;
                }
                continue;
            }
        };

        // Match native capture's dedup hash: clip_type, NUL, content bytes.
        let mut hash_material = Vec::new();
        hash_material.extend_from_slice(b"text");
        hash_material.push(0);
        hash_material.extend_from_slice(text.as_bytes());
        let content_hash = db.crypto.keyed_hash(&hash_material);

        let already: Option<String> =
            sqlx::query_scalar("SELECT uuid FROM clips WHERE content_hash = ?")
                .bind(&content_hash)
                .fetch_optional(&db.pool)
                .await
                .unwrap_or(None);
        if already.is_some() {
            result.duplicates += 1;
            continue;
        }

        if dry_run {
            result.imported += 1;
            continue;
        }

        let encrypted_content = match db.crypto.encrypt(text.as_bytes()) {
            Ok(value) => value,
            Err(error) => {
                result
                    .errors
                    .push(format!("clip {lid}: encrypt failed: {error}"));
                continue;
            }
        };
        let encrypted_preview = match db.crypto.encrypt_text(&truncate_preview(&text)) {
            Ok(value) => value,
            Err(error) => {
                result
                    .errors
                    .push(format!("clip {lid}: preview encrypt failed: {error}"));
                continue;
            }
        };
        let created_at = unix_to_datetime(ldate);
        let is_pinned: i64 = (dont_delete != 0) as i64;
        let uuid = Uuid::new_v4().to_string();

        let insert = sqlx::query(
            r#"
            INSERT INTO clips (uuid, clip_type, content, text_preview, content_hash, folder_id, is_deleted, is_thumbnail, source_app, source_icon, metadata, created_at, last_accessed, is_pinned)
            VALUES (?, 'text', ?, ?, ?, NULL, 0, 0, NULL, NULL, NULL, ?, ?, ?)
            "#,
        )
        .bind(&uuid)
        .bind(&encrypted_content)
        .bind(&encrypted_preview)
        .bind(&content_hash)
        .bind(&created_at)
        .bind(&created_at)
        .bind(is_pinned)
        .execute(&db.pool)
        .await;

        match insert {
            Ok(_) => result.imported += 1,
            Err(error) => result
                .errors
                .push(format!("clip {lid}: insert failed: {error}")),
        }
    }

    ditto.close().await;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16le(text: &str) -> Vec<u8> {
        let mut bytes: Vec<u8> = text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        bytes.extend_from_slice(&[0, 0]); // NUL terminator, as Ditto stores it
        bytes
    }

    #[test]
    fn decodes_unicode_text_and_strips_terminator() {
        assert_eq!(
            decode_utf16le(&utf16le("hello café 日本")).as_deref(),
            Some("hello café 日本")
        );
        assert_eq!(decode_utf16le(&utf16le("")), None);
        assert_eq!(decode_utf16le(&[0x41]), None); // odd length / too short
    }

    #[test]
    fn decodes_ansi_text() {
        assert_eq!(
            decode_ansi(b"powershell -File x\0").as_deref(),
            Some("powershell -File x")
        );
        assert_eq!(decode_ansi(b"\0"), None);
        assert_eq!(decode_ansi(b"   "), None);
    }

    #[test]
    fn prefers_unicode_over_ansi() {
        let formats = vec![
            ("CF_TEXT".to_string(), b"ansi\0".to_vec()),
            ("CF_UNICODETEXT".to_string(), utf16le("unicode")),
        ];
        assert_eq!(extract_text(&formats).as_deref(), Some("unicode"));
    }

    #[test]
    fn recognizes_image_formats() {
        assert!(is_image_format("PNG"));
        assert!(is_image_format("CF_DIB"));
        assert!(is_image_format("CF_DIBV5"));
        assert!(!is_image_format("CF_UNICODETEXT"));
        assert!(!is_image_format("HTML Format"));
    }

    #[test]
    fn converts_unix_seconds_to_sqlite_utc() {
        // 2021-01-01 00:00:00 UTC
        assert_eq!(unix_to_datetime(1609459200), "2021-01-01 00:00:00");
    }

    #[test]
    fn preview_is_truncated_by_chars_not_bytes() {
        let long = "é".repeat(300);
        assert_eq!(truncate_preview(&long).chars().count(), PREVIEW_LIMIT);
    }

    #[tokio::test]
    async fn imports_text_clips_with_dates_pins_and_dedup() {
        use crate::crypto::CryptoManager;
        use crate::database::Database;
        use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
        use std::sync::Arc;

        // Cubby destination DB (in-memory) with the real clips schema.
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("cubby db opens");
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
                is_thumbnail INTEGER NOT NULL DEFAULT 0,
                source_app TEXT,
                source_icon TEXT,
                metadata TEXT,
                created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
                last_accessed DATETIME DEFAULT CURRENT_TIMESTAMP,
                is_pinned INTEGER NOT NULL DEFAULT 0
            )
            "#,
        )
        .execute(&pool)
        .await
        .expect("clips table");
        let db = Database {
            pool,
            crypto: Arc::new(CryptoManager::ephemeral()),
            image_dir: std::env::temp_dir().join(format!("cubby-ditto-test-{}", Uuid::new_v4())),
        };

        // Synthetic Ditto source DB (temp file; the importer opens it read-only).
        let ditto_path = std::env::temp_dir().join(format!("ditto-src-{}.db", Uuid::new_v4()));
        {
            let ditto = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(
                    SqliteConnectOptions::new()
                        .filename(&ditto_path)
                        .create_if_missing(true)
                        .journal_mode(SqliteJournalMode::Delete),
                )
                .await
                .expect("ditto db creates");
            sqlx::query("CREATE TABLE Main(lID INTEGER PRIMARY KEY, lDate INTEGER, mText TEXT, lDontAutoDelete INTEGER, bIsGroup INTEGER)")
                .execute(&ditto).await.unwrap();
            sqlx::query("CREATE TABLE Data(lID INTEGER PRIMARY KEY, lParentID INTEGER, strClipBoardFormat TEXT, ooData BLOB)")
                .execute(&ditto).await.unwrap();

            async fn main_row(
                p: &sqlx::SqlitePool,
                id: i64,
                date: i64,
                mtext: &str,
                pin: i64,
                group: i64,
            ) {
                sqlx::query(
                    "INSERT INTO Main(lID,lDate,mText,lDontAutoDelete,bIsGroup) VALUES (?,?,?,?,?)",
                )
                .bind(id)
                .bind(date)
                .bind(mtext)
                .bind(pin)
                .bind(group)
                .execute(p)
                .await
                .unwrap();
            }
            async fn data_row(p: &sqlx::SqlitePool, parent: i64, fmt: &str, blob: Vec<u8>) {
                sqlx::query("INSERT INTO Data(lParentID,strClipBoardFormat,ooData) VALUES (?,?,?)")
                    .bind(parent)
                    .bind(fmt)
                    .bind(blob)
                    .execute(p)
                    .await
                    .unwrap();
            }

            main_row(&ditto, 1, 1609459200, "reset steps", 1, 0).await; // pinned unicode
            data_row(&ditto, 1, "CF_UNICODETEXT", utf16le("reset password steps")).await;
            main_row(&ditto, 2, 1609545600, "isql", 0, 0).await; // ansi text
            data_row(&ditto, 2, "CF_TEXT", b"isql -u SYSDBA\0".to_vec()).await;
            main_row(&ditto, 3, 1609600000, "My Group", 0, 1).await; // group -> skip
            main_row(&ditto, 4, 1609700000, "", 0, 0).await; // image -> skip
            data_row(&ditto, 4, "CF_DIB", vec![0u8; 40]).await;
            main_row(&ditto, 5, 1609800000, "https://example.com/kb/42", 0, 0).await; // mText only
            main_row(&ditto, 6, 1609900000, "dupe", 0, 0).await; // duplicate of #1
            data_row(&ditto, 6, "CF_UNICODETEXT", utf16le("reset password steps")).await;

            ditto.close().await;
        }

        let result = import_from_ditto(&db, ditto_path.to_str().unwrap(), false)
            .await
            .expect("import runs");
        assert_eq!(result.total, 6);
        assert_eq!(result.skipped_groups, 1);
        assert_eq!(result.skipped_images, 1);
        assert_eq!(result.imported, 3, "unicode + ansi + mText clips");
        assert_eq!(result.duplicates, 1);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        // The pinned clip decrypts to its original text and keeps its Ditto date.
        let (content, created): (Vec<u8>, String) =
            sqlx::query_as("SELECT content, created_at FROM clips WHERE is_pinned = 1")
                .fetch_one(&db.pool)
                .await
                .expect("one pinned clip");
        assert_eq!(
            db.crypto.decrypt(&content).unwrap(),
            b"reset password steps"
        );
        assert_eq!(created, "2021-01-01 00:00:00");

        // Re-running as a dry run against the now-populated DB imports nothing.
        let dry = import_from_ditto(&db, ditto_path.to_str().unwrap(), true)
            .await
            .unwrap();
        assert_eq!(dry.imported, 0);
        assert_eq!(dry.duplicates, 4);

        let _ = std::fs::remove_file(&ditto_path);
    }
}
