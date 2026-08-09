//! Encrypted local backup bundles (SOU-589).
//!
//! Moving history between machines, or recovering after a reinstall, previously
//! meant copying raw AppData files by hand — easy to get wrong, and SOU-227 is
//! what that looks like when it goes badly.
//!
//! # Why a passphrase, and not the storage key
//!
//! Clips are encrypted at rest under `storage.key`, which is DPAPI-protected
//! for one Windows account on one machine. A bundle encrypted with that key
//! could never be restored anywhere else, which defeats the point. So a bundle
//! is encrypted under a key derived from a passphrase the user supplies:
//! Argon2id over a random per-bundle salt, then AES-256-GCM — the same cipher
//! the local store already uses.
//!
//! Plaintext never reaches the disk. Clips are decrypted into memory, the
//! bundle is serialized in memory, and only the ciphertext is written.
//!
//! # Layout
//!
//! ```text
//! magic  8 bytes   b"CUBBAK01"
//! salt  16 bytes   Argon2id salt
//! nonce 12 bytes   AES-256-GCM nonce
//! body  ...        AES-256-GCM(JSON payload)
//! ```
//!
//! The magic doubles as the AEAD's associated data, so a bundle whose header
//! was edited fails to decrypt rather than being parsed as something else.

use crate::database::Database;
use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const MAGIC: &[u8; 8] = b"CUBBAK01";
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;
const HEADER_LEN: usize = MAGIC.len() + SALT_LEN + NONCE_LEN;

/// Rejects an obviously-wrong file before spending Argon2 time on it.
const MAX_BUNDLE_BYTES: u64 = 4 * 1024 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
struct BackupBundle {
    version: u32,
    exported_at: String,
    clips: Vec<BackupClip>,
}

#[derive(Debug, Serialize, Deserialize)]
struct BackupClip {
    clip_type: String,
    /// The clip's stored bytes, decrypted. For an image this is the thumbnail;
    /// see the note on `export_backup` about full-resolution blobs.
    content_b64: String,
    text_preview: String,
    is_pinned: bool,
    /// Defaulted so a bundle written before this field existed still imports;
    /// those clips were not hidden, which is what `false` says.
    #[serde(default)]
    is_hidden: bool,
    #[serde(default)]
    source_app: Option<String>,
    #[serde(default)]
    source_icon: Option<String>,
    #[serde(default)]
    metadata: Option<String>,
    #[serde(default)]
    ocr_text: Option<String>,
    created_at: String,
    /// Folder *name*, not id: ids are local to a database.
    #[serde(default)]
    folder: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct BackupImportResult {
    pub total: usize,
    pub imported: usize,
    pub duplicates: usize,
    pub errors: Vec<String>,
    pub dry_run: bool,
}

/// One row of the export query: clip fields plus the joined folder name.
type ExportRow = (
    String,                        // clip_type
    Vec<u8>,                       // content (encrypted)
    String,                        // text_preview (encrypted)
    bool,                          // is_pinned
    bool,                          // is_hidden
    Option<String>,                // source_app (encrypted)
    Option<String>,                // source_icon (encrypted)
    Option<String>,                // metadata (encrypted)
    Option<String>,                // ocr_text (encrypted)
    chrono::DateTime<chrono::Utc>, // created_at
    Option<String>,                // folder name
);

/// Argon2id parameters. The defaults are the RustCrypto recommendation; they
/// are pinned here because changing them silently would make every existing
/// bundle undecryptable.
fn derive_key(passphrase: &str, salt: &[u8]) -> Result<[u8; KEY_LEN], String> {
    if passphrase.is_empty() {
        return Err("A passphrase is required to encrypt or open a backup".to_string());
    }
    let params = Params::new(19_456, 2, 1, Some(KEY_LEN))
        .map_err(|e| format!("invalid key-derivation parameters: {e}"))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = [0_u8; KEY_LEN];
    argon
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|e| format!("failed to derive the backup key: {e}"))?;
    Ok(key)
}

/// Write every live clip to `path`, encrypted under `passphrase`.
///
/// Returns how many clips were written.
///
/// Respects the current history exactly as the app sees it: soft-deleted rows
/// are skipped, and whatever retention has already pruned is simply not there.
/// Pinned state travels with each clip.
///
/// Full-resolution image blobs are deliberately **not** included. They live in
/// separate files that would multiply the bundle's size, and an imported image
/// lands in the same "thumbnail and recognized text kept, full image gone"
/// state that retention already produces (SOU-244), which the app models as a
/// first-class case rather than as damage.
pub async fn export_backup(db: &Database, path: &str, passphrase: &str) -> Result<usize, String> {
    let rows: Vec<ExportRow> = sqlx::query_as(
        r#"
        SELECT clips.clip_type,
               clips.content,
               clips.text_preview,
               clips.is_pinned,
               clips.is_hidden,
               clips.source_app,
               clips.source_icon,
               clips.metadata,
               clips.ocr_text,
               clips.created_at,
               folders.name
        FROM clips
        LEFT JOIN folders ON folders.id = clips.folder_id
        WHERE clips.is_deleted = 0
        ORDER BY clips.created_at ASC
        "#,
    )
    .fetch_all(&db.pool)
    .await
    .map_err(|e| format!("Could not read the clip history: {e}"))?;

    let mut clips = Vec::with_capacity(rows.len());
    for (
        clip_type,
        content,
        text_preview,
        is_pinned,
        is_hidden,
        source_app,
        source_icon,
        metadata,
        ocr_text,
        created_at,
        folder,
    ) in rows
    {
        // Decrypt into memory only. Anything unreadable is skipped rather than
        // failing the whole export — one bad row should not cost the backup.
        let content = match db.crypto.decrypt(&content) {
            Ok(value) => value,
            Err(error) => {
                log::warn!("BACKUP: Skipping a clip whose content could not be read: {error}");
                continue;
            }
        };
        let text_preview = db.crypto.decrypt_text(&text_preview).unwrap_or_default();
        let decrypt_optional = |value: Option<String>| {
            value.and_then(|mut inner| {
                let mut holder = Some(std::mem::take(&mut inner));
                db.crypto.decrypt_optional_text(&mut holder).ok()?;
                holder
            })
        };

        clips.push(BackupClip {
            clip_type,
            content_b64: BASE64.encode(&content),
            text_preview,
            is_pinned,
            is_hidden,
            source_app: decrypt_optional(source_app),
            source_icon: decrypt_optional(source_icon),
            metadata: decrypt_optional(metadata),
            ocr_text: decrypt_optional(ocr_text),
            created_at: created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            folder,
        });
    }

    let bundle = BackupBundle {
        version: 1,
        exported_at: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        clips,
    };
    let count = bundle.clips.len();
    let plaintext =
        serde_json::to_vec(&bundle).map_err(|e| format!("Could not build the backup: {e}"))?;

    let mut salt = [0_u8; SALT_LEN];
    getrandom::fill(&mut salt).map_err(|e| format!("failed to generate a salt: {e}"))?;
    let mut nonce = [0_u8; NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(|e| format!("failed to generate a nonce: {e}"))?;

    let key = derive_key(passphrase, &salt)?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|e| format!("failed to prepare the cipher: {e}"))?;
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &plaintext,
                aad: MAGIC,
            },
        )
        .map_err(|_| "Could not encrypt the backup".to_string())?;

    let mut file = Vec::with_capacity(HEADER_LEN + ciphertext.len());
    file.extend_from_slice(MAGIC);
    file.extend_from_slice(&salt);
    file.extend_from_slice(&nonce);
    file.extend_from_slice(&ciphertext);

    std::fs::write(path, &file).map_err(|e| format!("Could not save the backup: {e}"))?;
    log::info!("BACKUP: Exported {count} clips to an encrypted bundle");
    Ok(count)
}

/// Read a bundle back in, skipping anything already present.
///
/// Dedup runs through the same content-hash lookup as capture and the Ditto
/// importer, and the hash is **recomputed locally** rather than carried in the
/// bundle: `keyed_hash` is keyed with this machine's storage key, so a hash
/// from the exporting machine would never match here.
pub async fn import_backup(
    db: &Database,
    path: &str,
    passphrase: &str,
    dry_run: bool,
) -> Result<BackupImportResult, String> {
    let metadata =
        std::fs::metadata(path).map_err(|e| format!("Could not open the backup file: {e}"))?;
    if metadata.len() > MAX_BUNDLE_BYTES {
        return Err("That backup file is implausibly large".to_string());
    }
    let raw = std::fs::read(path).map_err(|e| format!("Could not read the backup file: {e}"))?;
    if raw.len() < HEADER_LEN || &raw[..MAGIC.len()] != MAGIC {
        return Err("That does not look like a Cubby backup file".to_string());
    }

    let salt = &raw[MAGIC.len()..MAGIC.len() + SALT_LEN];
    let nonce = &raw[MAGIC.len() + SALT_LEN..HEADER_LEN];
    let key = derive_key(passphrase, salt)?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|e| format!("failed to prepare the cipher: {e}"))?;
    let plaintext = cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: &raw[HEADER_LEN..],
                aad: MAGIC,
            },
        )
        // A wrong passphrase and a tampered file are indistinguishable here, and
        // the wrong passphrase is overwhelmingly the likely one.
        .map_err(|_| "Wrong passphrase, or the backup file is damaged".to_string())?;

    let bundle: BackupBundle = serde_json::from_slice(&plaintext)
        .map_err(|e| format!("That backup could not be understood: {e}"))?;
    if bundle.version != 1 {
        return Err(format!(
            "That backup was written by a newer version of Cubby (format {})",
            bundle.version
        ));
    }

    let mut result = BackupImportResult {
        total: bundle.clips.len(),
        dry_run,
        ..Default::default()
    };
    // Guards against a bundle that contains the same clip twice: the database
    // lookup cannot see rows this same run has not committed yet.
    let mut planned = std::collections::HashSet::new();

    for clip in bundle.clips {
        let content = match BASE64.decode(clip.content_b64.as_bytes()) {
            Ok(bytes) => bytes,
            Err(_) => {
                result
                    .errors
                    .push("A clip's content was unreadable".to_string());
                continue;
            }
        };

        let hash_material = crate::clipboard::build_clip_hash_material(
            &clip.clip_type,
            &content,
            std::iter::empty::<(&str, &[u8])>(),
        );
        let content_hash = db.crypto.keyed_hash(&hash_material);

        let already: Option<String> =
            sqlx::query_scalar("SELECT uuid FROM clips WHERE content_hash = ?")
                .bind(&content_hash)
                .fetch_optional(&db.pool)
                .await
                .map_err(|e| format!("Could not check for an existing clip: {e}"))?;
        if already.is_some() || !planned.insert(content_hash.clone()) {
            result.duplicates += 1;
            continue;
        }

        if dry_run {
            result.imported += 1;
            continue;
        }

        let folder_id = match clip.folder.as_deref() {
            Some(name) => match ensure_folder(db, name).await {
                Ok(id) => Some(id),
                Err(error) => {
                    result.errors.push(error);
                    None
                }
            },
            None => None,
        };

        let encrypted_content = match db.crypto.encrypt(&content) {
            Ok(value) => value,
            Err(error) => {
                result.errors.push(format!("encrypt failed: {error}"));
                continue;
            }
        };
        let encrypted_preview = match db.crypto.encrypt_text(&clip.text_preview) {
            Ok(value) => value,
            Err(error) => {
                result
                    .errors
                    .push(format!("preview encrypt failed: {error}"));
                continue;
            }
        };
        let encrypt_optional =
            |value: Option<&str>| db.crypto.encrypt_optional_text(value).ok().flatten();

        // Images arrive as their thumbnail, which is exactly the state
        // retention leaves an expired image in, so mark them that way rather
        // than letting the app offer a full-resolution paste that cannot work.
        let full_image_expired = i64::from(clip.clip_type == "image");
        let ocr_status = if clip.ocr_text.is_some() {
            Some("completed")
        } else {
            None
        };

        let insert = sqlx::query(
            r#"
            INSERT INTO clips (
                uuid, clip_type, content, text_preview, content_hash, folder_id,
                is_deleted, is_thumbnail, source_app, source_icon, metadata,
                ocr_text, ocr_status, full_image_expired, created_at, last_accessed, is_pinned,
                is_hidden
            )
            VALUES (?, ?, ?, ?, ?, ?, 0, 0, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(Uuid::new_v4().to_string())
        .bind(&clip.clip_type)
        .bind(&encrypted_content)
        .bind(&encrypted_preview)
        .bind(&content_hash)
        .bind(folder_id)
        .bind(encrypt_optional(clip.source_app.as_deref()))
        .bind(encrypt_optional(clip.source_icon.as_deref()))
        .bind(encrypt_optional(clip.metadata.as_deref()))
        .bind(encrypt_optional(clip.ocr_text.as_deref()))
        .bind(ocr_status)
        .bind(full_image_expired)
        .bind(&clip.created_at)
        .bind(&clip.created_at)
        .bind(i64::from(clip.is_pinned))
        // Restored hidden. A clip the user chose to hide must not come back
        // visible: the column defaults to 0, so omitting this would silently
        // undo the protection on every round trip.
        .bind(i64::from(clip.is_hidden))
        .execute(&db.pool)
        .await;

        match insert {
            Ok(_) => result.imported += 1,
            Err(error) => result.errors.push(format!("insert failed: {error}")),
        }
    }

    if result.imported > 0 && !dry_run {
        // The in-memory index was built before these rows existed.
        db.search_index.invalidate();
    }
    Ok(result)
}

/// Find a folder by name, creating it if the bundle references one this machine
/// does not have. Names are how folders travel; ids are local to a database.
async fn ensure_folder(db: &Database, name: &str) -> Result<i64, String> {
    if let Some(id) = sqlx::query_scalar::<_, i64>("SELECT id FROM folders WHERE name = ?")
        .bind(name)
        .fetch_optional(&db.pool)
        .await
        .map_err(|e| format!("Could not look up folder {name}: {e}"))?
    {
        return Ok(id);
    }
    sqlx::query("INSERT INTO folders (name) VALUES (?)")
        .bind(name)
        .execute(&db.pool)
        .await
        .map(|done| done.last_insert_rowid())
        .map_err(|e| format!("Could not create folder {name}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("cubby-backup-{label}-{}.cubbybak", Uuid::new_v4()))
    }

    async fn test_database() -> Database {
        let database = Database {
            pool: sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect("sqlite::memory:")
                .await
                .expect("in-memory database should open"),
            crypto: std::sync::Arc::new(crate::crypto::CryptoManager::ephemeral()),
            image_dir: std::env::temp_dir().join(format!("cubby-backup-{}", Uuid::new_v4())),
            search_index: std::sync::Arc::new(crate::search_index::SearchIndex::default()),
        };
        database.migrate().await.expect("migration should succeed");
        database
    }

    async fn insert_clip(db: &Database, text: &str, pinned: bool, folder: Option<&str>) {
        let folder_id = match folder {
            Some(name) => Some(ensure_folder(db, name).await.unwrap()),
            None => None,
        };
        let material = crate::clipboard::build_clip_hash_material(
            "text",
            text.as_bytes(),
            std::iter::empty::<(&str, &[u8])>(),
        );
        sqlx::query(
            r#"INSERT INTO clips (uuid, clip_type, content, text_preview, content_hash, folder_id, is_pinned, created_at, last_accessed)
               VALUES (?, 'text', ?, ?, ?, ?, ?, '2026-05-01 09:00:00', '2026-05-01 09:00:00')"#,
        )
        .bind(Uuid::new_v4().to_string())
        .bind(db.crypto.encrypt(text.as_bytes()).unwrap())
        .bind(db.crypto.encrypt_text(text).unwrap())
        .bind(db.crypto.keyed_hash(&material))
        .bind(folder_id)
        .bind(i64::from(pinned))
        .execute(&db.pool)
        .await
        .unwrap();
    }

    /// Matched on `content_hash`, not on an encrypted column: encryption is
    /// randomized, so re-encrypting the same text yields different bytes and a
    /// comparison against `text_preview` would never match.
    async fn set_hidden(db: &Database, text: &str) {
        let material = crate::clipboard::build_clip_hash_material(
            "text",
            text.as_bytes(),
            std::iter::empty::<(&str, &[u8])>(),
        );
        let affected = sqlx::query("UPDATE clips SET is_hidden = 1 WHERE content_hash = ?")
            .bind(db.crypto.keyed_hash(&material))
            .execute(&db.pool)
            .await
            .unwrap()
            .rows_affected();
        assert_eq!(affected, 1, "the clip to hide should exist");
    }

    async fn hidden_flags(db: &Database) -> Vec<(String, bool)> {
        let rows: Vec<(Vec<u8>, bool)> =
            sqlx::query_as("SELECT content, is_hidden FROM clips WHERE is_deleted = 0 ORDER BY id")
                .fetch_all(&db.pool)
                .await
                .unwrap();
        rows.into_iter()
            .map(|(content, hidden)| {
                (
                    String::from_utf8(db.crypto.decrypt(&content).unwrap()).unwrap(),
                    hidden,
                )
            })
            .collect()
    }

    /// A clip the user hid must come back hidden. The column defaults to 0, so
    /// a bundle that does not carry the flag silently unhides everything it
    /// restores — the protection would be undone by a backup round trip.
    #[tokio::test]
    async fn hidden_survives_a_round_trip() {
        let source = test_database().await;
        insert_clip(&source, "a recovery code", false, None).await;
        insert_clip(&source, "an ordinary note", false, None).await;
        set_hidden(&source, "a recovery code").await;

        let path = temp_path("hidden");
        let path_str = path.to_string_lossy().to_string();
        export_backup(&source, &path_str, "correct horse")
            .await
            .unwrap();

        let target = test_database().await;
        import_backup(&target, &path_str, "correct horse", false)
            .await
            .unwrap();

        let mut restored = hidden_flags(&target).await;
        restored.sort();
        assert_eq!(
            restored,
            vec![
                ("a recovery code".to_string(), true),
                ("an ordinary note".to_string(), false),
            ],
            "the hidden flag must survive export and import"
        );
        let _ = std::fs::remove_file(&path);
    }

    async fn clip_texts(db: &Database) -> Vec<String> {
        let rows: Vec<Vec<u8>> =
            sqlx::query_scalar("SELECT content FROM clips WHERE is_deleted = 0 ORDER BY id")
                .fetch_all(&db.pool)
                .await
                .unwrap();
        rows.into_iter()
            .map(|value| String::from_utf8(db.crypto.decrypt(&value).unwrap()).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn round_trips_into_a_different_machine_and_dedups_on_reimport() {
        let source = test_database().await;
        insert_clip(&source, "alpha note", true, Some("Receipts")).await;
        insert_clip(&source, "beta note", false, None).await;

        let path = temp_path("roundtrip");
        let exported = export_backup(&source, path.to_str().unwrap(), "correct horse")
            .await
            .expect("export should succeed");
        assert_eq!(exported, 2);

        // A different database with a different storage key: this is the whole
        // point, and it is why the bundle cannot be encrypted with that key.
        let target = test_database().await;
        let result = import_backup(&target, path.to_str().unwrap(), "correct horse", false)
            .await
            .expect("import should succeed");
        assert_eq!(result.imported, 2);
        assert_eq!(result.duplicates, 0);
        assert!(result.errors.is_empty());

        let mut texts = clip_texts(&target).await;
        texts.sort();
        assert_eq!(texts, vec!["alpha note", "beta note"]);

        // Pinned state and folders travel with the clips.
        let pinned: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM clips WHERE is_pinned = 1")
            .fetch_one(&target.pool)
            .await
            .unwrap();
        assert_eq!(pinned, 1);
        let folders: Vec<String> = sqlx::query_scalar("SELECT name FROM folders")
            .fetch_all(&target.pool)
            .await
            .unwrap();
        assert_eq!(folders, vec!["Receipts".to_string()]);

        // Re-importing the same bundle adds nothing.
        let again = import_backup(&target, path.to_str().unwrap(), "correct horse", false)
            .await
            .expect("second import should succeed");
        assert_eq!(again.imported, 0);
        assert_eq!(again.duplicates, 2);
        assert_eq!(clip_texts(&target).await.len(), 2);

        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn a_wrong_passphrase_is_refused_and_the_bundle_holds_no_plaintext() {
        let source = test_database().await;
        insert_clip(&source, "swordfish token 8821", false, None).await;

        let path = temp_path("secrecy");
        export_backup(&source, path.to_str().unwrap(), "right one")
            .await
            .unwrap();

        // The file on disk must not contain the clip in the clear.
        let raw = std::fs::read(&path).unwrap();
        assert!(!raw
            .windows("swordfish".len())
            .any(|window| window == b"swordfish"));
        assert_eq!(&raw[..MAGIC.len()], MAGIC);

        let target = test_database().await;
        let wrong = import_backup(&target, path.to_str().unwrap(), "wrong one", false).await;
        assert!(wrong.is_err());
        assert_eq!(clip_texts(&target).await.len(), 0);

        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn a_tampered_header_is_refused() {
        let source = test_database().await;
        insert_clip(&source, "alpha", false, None).await;
        let path = temp_path("tamper");
        export_backup(&source, path.to_str().unwrap(), "pass")
            .await
            .unwrap();

        let mut raw = std::fs::read(&path).unwrap();
        // Flip a salt byte: the derived key changes, so this must not decrypt.
        raw[MAGIC.len()] ^= 0xff;
        std::fs::write(&path, &raw).unwrap();

        let target = test_database().await;
        assert!(
            import_backup(&target, path.to_str().unwrap(), "pass", false)
                .await
                .is_err()
        );

        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn a_dry_run_reports_without_writing() {
        let source = test_database().await;
        insert_clip(&source, "alpha", false, None).await;
        let path = temp_path("dryrun");
        export_backup(&source, path.to_str().unwrap(), "pass")
            .await
            .unwrap();

        let target = test_database().await;
        let result = import_backup(&target, path.to_str().unwrap(), "pass", true)
            .await
            .unwrap();
        assert!(result.dry_run);
        assert_eq!(result.imported, 1);
        assert_eq!(clip_texts(&target).await.len(), 0);

        std::fs::remove_file(&path).ok();
    }

    #[tokio::test]
    async fn a_file_that_is_not_a_bundle_is_named_as_such() {
        let path = temp_path("bogus");
        std::fs::write(&path, b"just some bytes that are not a bundle").unwrap();
        let target = test_database().await;
        let error = import_backup(&target, path.to_str().unwrap(), "pass", false)
            .await
            .unwrap_err();
        assert!(error.contains("does not look like a Cubby backup"));
        std::fs::remove_file(&path).ok();
    }
}
