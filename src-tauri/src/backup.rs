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
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
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
    /// The clip's stored bytes, decrypted. For an image this is the 320×220
    /// thumbnail in `clips.content`, not the original.
    content_b64: String,
    text_preview: String,
    is_pinned: bool,
    /// Defaulted so a bundle written before this field existed still imports;
    /// those clips were not hidden, which is what `false` says.
    #[serde(default)]
    is_hidden: bool,
    /// Defaulted like the rest: a bundle written before notes existed simply
    /// has none, which is what None says.
    #[serde(default)]
    notes: Option<String>,
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
    /// Full-resolution PNG, decrypted. Present when the source clip still had
    /// a live `{uuid}.cubby` original. Absent for text, for images retention
    /// already expired, and for bundles written before SBS-919.
    #[serde(default)]
    full_image_b64: Option<String>,
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
    String,                        // uuid (diagnostics only; never clip contents)
    String,                        // clip_type
    Vec<u8>,                       // content (encrypted)
    String,                        // text_preview (encrypted)
    bool,                          // is_pinned
    bool,                          // is_hidden
    Option<String>,                // notes (encrypted)
    Option<String>,                // source_app (encrypted)
    Option<String>,                // source_icon (encrypted)
    Option<String>,                // metadata (encrypted)
    Option<String>,                // ocr_text (encrypted)
    chrono::DateTime<chrono::Utc>, // created_at
    bool,                          // full_image_expired
    Option<String>,                // folder name
);

/// How many failing clip ids the refusal message names. A database-wide key
/// problem fails every row, and a message with thousands of ids in it is a
/// message nobody can read.
const MAX_REPORTED_FAILED_CLIPS: usize = 5;

/// Counts of clips that could not be fully decrypted, grouped by field, plus
/// the ids of the first few offending rows.
///
/// Field names are storage-column names and the ids are the `clips.uuid`
/// diagnostics identifier — never clipboard contents. SBS-772.
#[derive(Debug, Default)]
struct ExportDecryptFailures {
    clips: usize,
    fields: BTreeMap<&'static str, usize>,
    uuids: Vec<String>,
}

impl ExportDecryptFailures {
    fn record(&mut self, uuid: &str, fields: &[&'static str]) {
        if fields.is_empty() {
            return;
        }
        self.clips += 1;
        if self.uuids.len() < MAX_REPORTED_FAILED_CLIPS {
            self.uuids.push(uuid.to_string());
        }
        for field in fields {
            *self.fields.entry(*field).or_insert(0) += 1;
        }
    }

    fn is_empty(&self) -> bool {
        self.clips == 0
    }

    /// Names the offending rows. Without an id the user sees the same list of
    /// clips as before — `decrypt_clip_fields` drops an unreadable note rather
    /// than hiding the clip — and has no way to tell which one blocks the
    /// backup, let alone fix or delete it.
    fn describe(&self) -> String {
        let details = self
            .fields
            .iter()
            .map(|(field, count)| format!("{count} {}", export_field_label(field)))
            .collect::<Vec<_>>()
            .join(", ");
        let clip_word = if self.clips == 1 { "clip" } else { "clips" };
        let id_word = if self.clips == 1 {
            "Affected clip id"
        } else {
            "Affected clip ids"
        };
        let mut ids = self.uuids.join(", ");
        let unlisted = self.clips.saturating_sub(self.uuids.len());
        if unlisted > 0 {
            ids.push_str(&format!(", and {unlisted} more"));
        }
        format!(
            "Could not write a complete backup: {} {clip_word} could not be fully decrypted ({details}). {id_word}: {ids}. The destination file was left unchanged.",
            self.clips
        )
    }
}

fn export_field_label(field: &str) -> &'static str {
    match field {
        "content" => "unreadable clip payload",
        "text_preview" => "unreadable preview",
        "notes" => "unreadable notes",
        "source_app" => "unreadable source app",
        "source_icon" => "unreadable source icon",
        "metadata" => "unreadable rich format",
        "ocr_text" => "unreadable recognized text",
        "full_image" => "unreadable full-resolution image",
        _ => "unreadable field",
    }
}

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
/// A successful export is a complete export. Any live clip field that cannot
/// be fully decrypted fails the whole operation (SBS-772): the destination
/// file is left unchanged and any temporary output is removed. Partial export
/// is not offered from this path.
///
/// Live screenshot originals travel with the bundle (SBS-919). `clips.content`
/// is only the 320×220 thumbnail; the PNG lives in `{uuid}.cubby`. An image
/// that is not already expired must include those bytes, or the export fails
/// the same way an unreadable field does. Images retention has already expired
/// stay thumbnail-only, which is the first-class SOU-244 state rather than
/// damage.
pub async fn export_backup(db: &Database, path: &str, passphrase: &str) -> Result<usize, String> {
    let rows: Vec<ExportRow> = sqlx::query_as(
        r#"
        SELECT clips.uuid,
               clips.clip_type,
               clips.content,
               clips.text_preview,
               clips.is_pinned,
               clips.is_hidden,
               clips.notes,
               clips.source_app,
               clips.source_icon,
               clips.metadata,
               clips.ocr_text,
               clips.created_at,
               clips.full_image_expired,
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
    let mut failures = ExportDecryptFailures::default();
    for (
        uuid,
        clip_type,
        content,
        text_preview,
        is_pinned,
        is_hidden,
        notes,
        source_app,
        source_icon,
        metadata,
        ocr_text,
        created_at,
        full_image_expired,
        folder,
    ) in rows
    {
        // Decrypt into memory only. A backup that omits an unreadable field
        // still looks successful to the user, so one bad field fails the export
        // rather than writing an incomplete bundle. SBS-772.
        match decrypt_export_clip(
            &db.crypto,
            &uuid,
            clip_type,
            content,
            text_preview,
            is_pinned,
            is_hidden,
            notes,
            source_app,
            source_icon,
            metadata,
            ocr_text,
            created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            folder,
        ) {
            Ok(clip) => match attach_export_full_image(db, &uuid, clip, full_image_expired).await {
                Ok(clip) => clips.push(clip),
                Err(fields) => failures.record(&uuid, &fields),
            },
            Err(fields) => failures.record(&uuid, &fields),
        }
    }

    if !failures.is_empty() {
        let message = failures.describe();
        log::error!("BACKUP: {message}");
        return Err(message);
    }

    let bundle = BackupBundle {
        version: 1,
        exported_at: chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        clips,
    };
    let count = bundle.clips.len();
    let file = seal_bundle(&bundle, passphrase)?;

    persist_backup_file(path, &file)?;
    log::info!("BACKUP: Exported {count} clips to an encrypted bundle");
    Ok(count)
}

/// Serialize and encrypt a bundle into the on-disk file bytes.
///
/// Split out of `export_backup` so a test can seal a bundle this database would
/// never produce, which is the only way to exercise the import guards against a
/// crafted file.
fn seal_bundle(bundle: &BackupBundle, passphrase: &str) -> Result<Vec<u8>, String> {
    let plaintext =
        serde_json::to_vec(bundle).map_err(|e| format!("Could not build the backup: {e}"))?;

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
    Ok(file)
}

#[allow(clippy::too_many_arguments)]
fn decrypt_export_clip(
    crypto: &crate::crypto::CryptoManager,
    uuid: &str,
    clip_type: String,
    content: Vec<u8>,
    text_preview: String,
    is_pinned: bool,
    is_hidden: bool,
    notes: Option<String>,
    source_app: Option<String>,
    source_icon: Option<String>,
    metadata: Option<String>,
    ocr_text: Option<String>,
    created_at: String,
    folder: Option<String>,
) -> Result<BackupClip, Vec<&'static str>> {
    let mut failed_fields = Vec::new();

    let content = match crypto.decrypt(&content) {
        Ok(value) => Some(value),
        Err(error) => {
            log::warn!("BACKUP: clip {uuid} content could not be decrypted: {error}");
            failed_fields.push("content");
            None
        }
    };
    let text_preview = match crypto.decrypt_text(&text_preview) {
        Ok(value) => Some(value),
        Err(error) => {
            log::warn!("BACKUP: clip {uuid} text_preview could not be decrypted: {error}");
            failed_fields.push("text_preview");
            None
        }
    };

    let notes = decrypt_optional_export_field(crypto, uuid, notes, "notes", &mut failed_fields);
    let source_app =
        decrypt_optional_export_field(crypto, uuid, source_app, "source_app", &mut failed_fields);
    let source_icon =
        decrypt_optional_export_field(crypto, uuid, source_icon, "source_icon", &mut failed_fields);
    let metadata =
        decrypt_optional_export_field(crypto, uuid, metadata, "metadata", &mut failed_fields);
    let ocr_text =
        decrypt_optional_export_field(crypto, uuid, ocr_text, "ocr_text", &mut failed_fields);

    if !failed_fields.is_empty() {
        return Err(failed_fields);
    }

    let (Some(content), Some(text_preview)) = (content, text_preview) else {
        return Err(vec!["content"]);
    };

    Ok(BackupClip {
        clip_type,
        content_b64: BASE64.encode(&content),
        text_preview,
        is_pinned,
        is_hidden,
        notes,
        source_app,
        source_icon,
        metadata,
        ocr_text,
        created_at,
        folder,
        full_image_b64: None,
    })
}

fn decrypt_optional_export_field(
    crypto: &crate::crypto::CryptoManager,
    uuid: &str,
    value: Option<String>,
    field: &'static str,
    failed_fields: &mut Vec<&'static str>,
) -> Option<String> {
    let inner = value?;
    let mut holder = Some(inner);
    match crypto.decrypt_optional_text(&mut holder) {
        Ok(()) => holder,
        Err(error) => {
            log::warn!("BACKUP: clip {uuid} {field} could not be decrypted: {error}");
            failed_fields.push(field);
            None
        }
    }
}

/// Attach the live `{uuid}.cubby` original, or fail the clip the same way an
/// unreadable field does. Already-expired images stay thumbnail-only.
async fn attach_export_full_image(
    db: &Database,
    uuid: &str,
    mut clip: BackupClip,
    full_image_expired: bool,
) -> Result<BackupClip, Vec<&'static str>> {
    if clip.clip_type != "image" || full_image_expired {
        return Ok(clip);
    }
    match load_export_full_image(db, uuid).await {
        Ok(bytes) => {
            clip.full_image_b64 = Some(BASE64.encode(&bytes));
            Ok(clip)
        }
        Err(error) => {
            let now_expired = sqlx::query_scalar::<_, bool>(
                "SELECT full_image_expired FROM clips WHERE uuid = ?",
            )
            .bind(uuid)
            .fetch_optional(&db.pool)
            .await
            .ok()
            .flatten()
            .unwrap_or(false);
            if now_expired {
                return Ok(clip);
            }
            log::warn!("BACKUP: clip {uuid} full-resolution original could not be read: {error}");
            Err(vec!["full_image"])
        }
    }
}

/// Read the live original for export.
///
/// Never falls back to `clips.content`. That column is the thumbnail, and
/// treating it as the original is the SBS-919 loss: the bundle looks complete,
/// import marks the clip expired, and the screenshot is gone.
///
/// A query error, a decrypt error, and a missing file are distinct. Only a
/// confirmed empty/absent index plus a confirmed-absent managed file plus an
/// empty legacy blob means "nothing there" — and for a non-expired image that
/// is still a failure, because the original was supposed to exist.
async fn load_export_full_image(db: &Database, uuid: &str) -> Result<Vec<u8>, String> {
    let index: Option<(Option<String>, Vec<u8>)> =
        sqlx::query_as("SELECT file_path, full_content FROM clip_images WHERE clip_uuid = ?")
            .bind(uuid)
            .fetch_optional(&db.pool)
            .await
            .map_err(|error| format!("could not look up the stored original: {error}"))?;

    if let Some((file_path, _)) = &index {
        if let Some(path) = file_path.as_deref().filter(|path| !path.is_empty()) {
            match read_export_image_file(&db.crypto, path) {
                Ok(bytes) => return Ok(bytes),
                Err(error) => log::warn!(
                    "BACKUP: clip {uuid} indexed original at {path} was unusable: {error}"
                ),
            }
        }
    }

    let managed = db.image_dir.join(format!("{uuid}.cubby"));
    let mut managed_error = None;
    match read_export_image_file(&db.crypto, &managed.to_string_lossy()) {
        Ok(bytes) => return Ok(bytes),
        Err(error) if error == "not-found" => {}
        Err(error) => managed_error = Some(error),
    }

    if let Some((_, full_content)) = index {
        if !full_content.is_empty() {
            let bytes = if db.crypto.is_encrypted(&full_content) {
                db.crypto.decrypt(&full_content)?
            } else {
                full_content
            };
            if bytes.is_empty() {
                return Err("decrypted original was empty".to_string());
            }
            return Ok(bytes);
        }
    }

    if let Some(error) = managed_error {
        return Err(format!(
            "managed original {} could not be read: {error}",
            managed.display()
        ));
    }
    Err("full-resolution original is missing".to_string())
}

fn read_export_image_file(
    crypto: &crate::crypto::CryptoManager,
    path: &str,
) -> Result<Vec<u8>, String> {
    let encrypted = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err("not-found".to_string());
        }
        Err(error) => return Err(error.to_string()),
    };
    if encrypted.is_empty() {
        return Err("file was empty".to_string());
    }
    let bytes = crypto.decrypt(&encrypted)?;
    if bytes.is_empty() {
        return Err("decrypted original was empty".to_string());
    }
    Ok(bytes)
}

/// Write `bytes` beside `path`, then replace the destination so a failed
/// persist cannot leave a truncated backup in the user's chosen file.
fn persist_backup_file(path: &str, bytes: &[u8]) -> Result<(), String> {
    let dest = Path::new(path);
    if dest.file_name().is_none() {
        return Err("Could not save the backup: the destination path is not a file".to_string());
    }
    let temp = backup_temp_path(dest);

    if let Err(error) = std::fs::write(&temp, bytes) {
        remove_backup_temp(&temp);
        return Err(format!("Could not save the backup: {error}"));
    }

    let Err(error) = replace_exported_backup(&temp, dest) else {
        return Ok(());
    };

    // On exFAT/FAT — a realistic place to save a `.cubbybak` — replacing is
    // delete-the-target-then-rename rather than the atomic rename-over NTFS
    // gives us. A failure after the destination entry is gone leaves the temp
    // holding the only copy of the bundle, so deleting it here would destroy
    // both the old backup and the new one. Try to put it in place instead.
    if !dest.exists() && temp.exists() && std::fs::rename(&temp, dest).is_ok() {
        log::warn!(
            "BACKUP: replacing the destination failed ({error}); the new bundle was renamed into place instead"
        );
        return Ok(());
    }

    remove_backup_temp(&temp);
    Err(format!("Could not save the backup: {error}"))
}

/// Best-effort removal of the sibling temp, retried briefly.
///
/// The temp holds bundle bytes, so leaving one behind leaks history beside the
/// destination. A virus scanner that is still reading the file makes the first
/// delete fail with a sharing violation and succeed a moment later.
fn remove_backup_temp(temp: &Path) {
    const ATTEMPTS: u32 = 5;
    for attempt in 1..=ATTEMPTS {
        match std::fs::remove_file(temp) {
            Ok(()) => return,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) if attempt == ATTEMPTS => {
                log::error!(
                    "BACKUP: could not remove the temporary export file {}: {error}",
                    temp.display()
                );
                return;
            }
            Err(_) => std::thread::sleep(std::time::Duration::from_millis(20 * u64::from(attempt))),
        }
    }
}

/// A short sibling name. It deliberately does **not** embed the destination
/// file name: `MoveFileExW` is bounded by `MAX_PATH` on the source as well as
/// the destination, and a name that repeated a long destination pushed a legal
/// save-dialog path over the limit.
fn backup_temp_path(dest: &Path) -> PathBuf {
    let temp_name = format!(".{}.{}.tmp", std::process::id(), Uuid::new_v4());
    match dest.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(temp_name),
        _ => PathBuf::from(temp_name),
    }
}

/// `\\?\`-prefixed wide form of `path`, NUL terminated.
///
/// `MoveFileExW` fails above `MAX_PATH` unless the path is verbatim, while
/// `std::fs` prefixes long paths itself. Without this, a destination that
/// `std::fs::write` would have accepted is refused here.
#[cfg(target_os = "windows")]
fn verbatim_wide(path: &Path) -> Result<Vec<u16>, String> {
    use std::os::windows::ffi::OsStrExt;
    use std::path::{Component, Prefix};

    // `\\?\` requires a fully-qualified path with no `.` or `..` components.
    let absolute = std::path::absolute(path)
        .map_err(|error| format!("could not resolve {}: {error}", path.display()))?;
    let raw = absolute
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();

    let prefix = match absolute.components().next() {
        Some(Component::Prefix(prefix)) => prefix.kind(),
        // No prefix at all is not something GetFullPathNameW produces; pass it
        // through rather than inventing a shape for it.
        _ => return Ok(raw),
    };
    // Already verbatim, and `\\.\` device paths must not be rewritten.
    if prefix.is_verbatim() || matches!(prefix, Prefix::DeviceNS(_)) {
        return Ok(raw);
    }
    // `\\server\share\...` becomes `\\?\UNC\server\share\...`.
    if matches!(prefix, Prefix::UNC(..)) {
        let mut wide = r"\\?\UNC".encode_utf16().collect::<Vec<_>>();
        wide.extend_from_slice(&raw[1..]);
        return Ok(wide);
    }
    let mut wide = r"\\?\".encode_utf16().collect::<Vec<_>>();
    wide.extend_from_slice(&raw);
    Ok(wide)
}

#[cfg(target_os = "windows")]
fn replace_exported_backup(source: &Path, destination: &Path) -> Result<(), String> {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source_wide = verbatim_wide(source)?;
    let destination_wide = verbatim_wide(destination)?;

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
fn replace_exported_backup(source: &Path, destination: &Path) -> Result<(), String> {
    std::fs::rename(source, destination).map_err(|error| error.to_string())
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
        // Only an image clip can own a full-resolution original. A bundle that
        // pairs full_image_b64 with any other clip_type is crafted or buggy, so
        // drop it here: that keeps a stray PNG out of the image directory, out
        // of clip_images, and out of the content hash below. SBS-919.
        let full_image_b64 = if clip.clip_type == "image" {
            clip.full_image_b64.as_deref()
        } else {
            None
        };
        let full_image = match decode_optional_full_image(full_image_b64) {
            Ok(bytes) => bytes,
            Err(error) => {
                result.errors.push(error);
                continue;
            }
        };

        // Capture hashes an image from the full PNG, not the thumbnail. A
        // restored original has to use the same material or recopying that
        // screenshot on the new machine stores a duplicate.
        let hash_material = crate::clipboard::build_clip_hash_material(
            &clip.clip_type,
            full_image.as_deref().unwrap_or(&content),
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

        let new_uuid = Uuid::new_v4().to_string();
        // Persist the original before the row exists. A clip that lands without
        // its file would be marked expired and look like a successful restore
        // of a screenshot the user can no longer copy. SBS-919.
        let restored_original = match full_image.as_deref() {
            Some(bytes) => match persist_imported_original(db, &new_uuid, bytes) {
                Ok(path) => Some(path),
                Err(error) => {
                    result.errors.push(error);
                    continue;
                }
            },
            None => None,
        };
        // Thumbnail-only images (old bundles, or already expired) stay in the
        // first-class expired state so Copy image stays disabled.
        let full_image_expired =
            i64::from(clip.clip_type == "image" && restored_original.is_none());
        // Visible history date stays the source created_at. last_accessed is
        // the dest keep-for clock for live originals, so a 60-day-old
        // screenshot is not age-swept on the next capture after a PC migration.
        let last_accessed = if restored_original.is_some() {
            chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
        } else {
            clip.created_at.clone()
        };
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
                is_hidden, notes
            )
            VALUES (?, ?, ?, ?, ?, ?, 0, 0, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )
        .bind(&new_uuid)
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
        .bind(&last_accessed)
        .bind(i64::from(clip.is_pinned))
        // Restored hidden. A clip the user chose to hide must not come back
        // visible: the column defaults to 0, so omitting this would silently
        // undo the protection on every round trip.
        .bind(i64::from(clip.is_hidden))
        // Encrypted on the way back in, like every other text column.
        .bind(encrypt_optional(clip.notes.as_deref()))
        .execute(&db.pool)
        .await;

        match insert {
            Ok(_) => {
                if let Some(file_path) = restored_original {
                    if let Err(error) = index_imported_original(
                        db,
                        &new_uuid,
                        &file_path,
                        full_image.as_ref().map(Vec::len).unwrap_or(0) as i64,
                    )
                    .await
                    {
                        if let Err(cleanup) = sqlx::query("DELETE FROM clips WHERE uuid = ?")
                            .bind(&new_uuid)
                            .execute(&db.pool)
                            .await
                        {
                            log::error!(
                                "BACKUP: failed to roll back clip {new_uuid} after image index error: {cleanup}"
                            );
                        } else {
                            remove_imported_original(&file_path);
                        }
                        result.errors.push(error);
                        continue;
                    }
                }
                result.imported += 1;
            }
            Err(error) => {
                if let Some(file_path) = restored_original {
                    remove_imported_original(&file_path);
                }
                result.errors.push(format!("insert failed: {error}"));
            }
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

fn decode_optional_full_image(value: Option<&str>) -> Result<Option<Vec<u8>>, String> {
    let Some(encoded) = value else {
        return Ok(None);
    };
    let bytes = BASE64
        .decode(encoded.as_bytes())
        .map_err(|_| "A clip's full-resolution image was unreadable".to_string())?;
    if bytes.is_empty() {
        return Err("A clip's full-resolution image was empty".to_string());
    }
    Ok(Some(bytes))
}

fn persist_imported_original(
    db: &Database,
    uuid: &str,
    png_bytes: &[u8],
) -> Result<String, String> {
    crate::clipboard::persist_full_image_file(&db.crypto, &db.image_dir, uuid, png_bytes)
        .map_err(|error| format!("Could not restore a screenshot original: {error}"))
}

async fn index_imported_original(
    db: &Database,
    uuid: &str,
    file_path: &str,
    file_size: i64,
) -> Result<(), String> {
    sqlx::query(
        r#"
        INSERT INTO clip_images (clip_uuid, full_content, file_path, file_size, storage_kind, mime_type, created_at)
        VALUES (?, x'', ?, ?, 'file', 'image/png', CURRENT_TIMESTAMP)
        "#,
    )
    .bind(uuid)
    .bind(file_path)
    .bind(file_size)
    .execute(&db.pool)
    .await
    .map(|_| ())
    .map_err(|error| format!("Could not index a restored screenshot original: {error}"))
}

fn remove_imported_original(file_path: &str) {
    crate::clipboard::remove_full_image_file(file_path);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every export gets its own directory. The sibling temp no longer embeds
    /// the destination file name, so "did this export leave a temp behind?" is
    /// only answerable by looking at a directory nothing else writes to.
    fn temp_path(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cubby-backup-{label}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("the test directory should be creatable");
        dir.join("history.cubbybak")
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

    async fn set_note(db: &Database, text: &str, note: &str) {
        let material = crate::clipboard::build_clip_hash_material(
            "text",
            text.as_bytes(),
            std::iter::empty::<(&str, &[u8])>(),
        );
        let affected = sqlx::query("UPDATE clips SET notes = ? WHERE content_hash = ?")
            .bind(db.crypto.encrypt_optional_text(Some(note)).unwrap())
            .bind(db.crypto.keyed_hash(&material))
            .execute(&db.pool)
            .await
            .unwrap()
            .rows_affected();
        assert_eq!(affected, 1, "the clip to annotate should exist");
    }

    async fn notes_of(db: &Database) -> Vec<Option<String>> {
        let rows: Vec<Option<String>> =
            sqlx::query_scalar("SELECT notes FROM clips WHERE is_deleted = 0 ORDER BY id")
                .fetch_all(&db.pool)
                .await
                .unwrap();
        rows.into_iter()
            .map(|value| {
                let mut holder = value;
                db.crypto.decrypt_optional_text(&mut holder).ok();
                holder
            })
            .collect()
    }

    /// A note is the only record of why a clip was worth keeping, so losing it
    /// on a round trip quietly destroys the thing that made the clip findable.
    #[tokio::test]
    async fn notes_survive_a_round_trip() {
        let source = test_database().await;
        insert_clip(&source, "9f2c1b7e-40aa-4f11-b0d2-77c9e1f00a31", false, None).await;
        insert_clip(&source, "an unannotated clip", false, None).await;
        set_note(
            &source,
            "9f2c1b7e-40aa-4f11-b0d2-77c9e1f00a31",
            "staging api key",
        )
        .await;

        let path = temp_path("notes");
        let path_str = path.to_string_lossy().to_string();
        export_backup(&source, &path_str, "correct horse")
            .await
            .unwrap();

        let target = test_database().await;
        import_backup(&target, &path_str, "correct horse", false)
            .await
            .unwrap();

        let mut restored = notes_of(&target).await;
        restored.sort();
        assert_eq!(
            restored,
            vec![None, Some("staging api key".to_string())],
            "the note must survive export and import"
        );

        // And it must not sit in the clear in the restored database.
        let raw: Vec<Option<String>> =
            sqlx::query_scalar("SELECT notes FROM clips WHERE notes IS NOT NULL")
                .fetch_all(&target.pool)
                .await
                .unwrap();
        assert!(
            raw.iter().flatten().all(|value| !value.contains("staging")),
            "the restored note must be encrypted at rest"
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

    fn clip_hash(db: &Database, text: &str) -> String {
        let material = crate::clipboard::build_clip_hash_material(
            "text",
            text.as_bytes(),
            std::iter::empty::<(&str, &[u8])>(),
        );
        db.crypto.keyed_hash(&material)
    }

    async fn insert_rich_clip(db: &Database, text: &str) {
        insert_clip(db, text, false, None).await;
        let affected = sqlx::query(
            r#"UPDATE clips SET
                notes = ?,
                source_app = ?,
                source_icon = ?,
                metadata = ?,
                ocr_text = ?
               WHERE content_hash = ?"#,
        )
        .bind(
            db.crypto
                .encrypt_optional_text(Some("keep this note"))
                .unwrap(),
        )
        .bind(db.crypto.encrypt_optional_text(Some("Notepad")).unwrap())
        .bind(db.crypto.encrypt_optional_text(Some("icon-bytes")).unwrap())
        .bind(
            db.crypto
                .encrypt_optional_text(Some("<html>rich</html>"))
                .unwrap(),
        )
        .bind(
            db.crypto
                .encrypt_optional_text(Some("recognized words"))
                .unwrap(),
        )
        .bind(clip_hash(db, text))
        .execute(&db.pool)
        .await
        .unwrap()
        .rows_affected();
        assert_eq!(affected, 1, "the rich clip should exist");
    }

    async fn corrupt_content(db: &Database, text: &str) {
        let affected = sqlx::query("UPDATE clips SET content = ? WHERE content_hash = ?")
            .bind(b"not-an-encrypted-payload".to_vec())
            .bind(clip_hash(db, text))
            .execute(&db.pool)
            .await
            .unwrap()
            .rows_affected();
        assert_eq!(affected, 1, "the clip to corrupt should exist");
    }

    async fn corrupt_text_column(db: &Database, text: &str, column: &str) {
        let garbage = "CUB1:not-valid-ciphertext";
        let query = match column {
            "text_preview" => "UPDATE clips SET text_preview = ? WHERE content_hash = ?",
            "notes" => "UPDATE clips SET notes = ? WHERE content_hash = ?",
            "source_app" => "UPDATE clips SET source_app = ? WHERE content_hash = ?",
            "source_icon" => "UPDATE clips SET source_icon = ? WHERE content_hash = ?",
            "metadata" => "UPDATE clips SET metadata = ? WHERE content_hash = ?",
            "ocr_text" => "UPDATE clips SET ocr_text = ? WHERE content_hash = ?",
            other => panic!("unknown export column {other}"),
        };
        let affected = sqlx::query(query)
            .bind(garbage)
            .bind(clip_hash(db, text))
            .execute(&db.pool)
            .await
            .unwrap()
            .rows_affected();
        assert_eq!(affected, 1, "the clip to corrupt should exist");
    }

    fn leftover_backup_temps(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with('.') && name.ends_with(".tmp"))
            })
            .collect()
    }

    async fn uuid_of(db: &Database, text: &str) -> String {
        sqlx::query_scalar("SELECT uuid FROM clips WHERE content_hash = ?")
            .bind(clip_hash(db, text))
            .fetch_one(&db.pool)
            .await
            .expect("the clip should exist")
    }

    async fn insert_image_clip(
        db: &Database,
        thumbnail: &[u8],
        full: Option<&[u8]>,
        expired: bool,
    ) -> String {
        let uuid = Uuid::new_v4().to_string();
        let hash_input = full.unwrap_or(thumbnail);
        let material = crate::clipboard::build_clip_hash_material(
            "image",
            hash_input,
            std::iter::empty::<(&str, &[u8])>(),
        );
        sqlx::query(
            r#"INSERT INTO clips (
                uuid, clip_type, content, text_preview, content_hash,
                full_image_expired, created_at, last_accessed
            ) VALUES (?, 'image', ?, ?, ?, ?, '2026-05-01 09:00:00', '2026-05-01 09:00:00')"#,
        )
        .bind(&uuid)
        .bind(db.crypto.encrypt(thumbnail).unwrap())
        .bind(db.crypto.encrypt_text("[Image]").unwrap())
        .bind(db.crypto.keyed_hash(&material))
        .bind(i64::from(expired))
        .execute(&db.pool)
        .await
        .unwrap();
        if let Some(png) = full {
            std::fs::create_dir_all(&db.image_dir).unwrap();
            let file_path =
                crate::clipboard::persist_full_image_file(&db.crypto, &db.image_dir, &uuid, png)
                    .unwrap();
            sqlx::query(
                r#"INSERT INTO clip_images (
                    clip_uuid, full_content, file_path, file_size, storage_kind, mime_type
                ) VALUES (?, x'', ?, ?, 'file', 'image/png')"#,
            )
            .bind(&uuid)
            .bind(&file_path)
            .bind(png.len() as i64)
            .execute(&db.pool)
            .await
            .unwrap();
        }
        uuid
    }

    async fn restored_image_state(db: &Database) -> (bool, Option<Vec<u8>>, Vec<u8>) {
        let (uuid, expired, thumbnail): (String, bool, Vec<u8>) = sqlx::query_as(
            "SELECT uuid, full_image_expired, content FROM clips WHERE clip_type = 'image'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        let thumbnail = db.crypto.decrypt(&thumbnail).unwrap();
        let file_path: Option<String> =
            sqlx::query_scalar("SELECT file_path FROM clip_images WHERE clip_uuid = ?")
                .bind(&uuid)
                .fetch_optional(&db.pool)
                .await
                .unwrap();
        let original = file_path.map(|path| {
            crate::clipboard::read_full_image_file(&db.crypto, &path)
                .expect("original should decrypt")
        });
        (expired, original, thumbnail)
    }

    /// SBS-772: a clip whose payload cannot be decrypted must refuse the export
    /// rather than write a bundle that looks complete while omitting that clip.
    #[tokio::test]
    async fn export_refuses_a_corrupt_clip_payload_and_does_not_write_the_destination() {
        let source = test_database().await;
        insert_clip(&source, "sbs-772-payload-secret", false, None).await;
        insert_clip(&source, "a readable neighbor", false, None).await;
        corrupt_content(&source, "sbs-772-payload-secret").await;

        let path = temp_path("corrupt-payload");
        let error = export_backup(&source, path.to_str().unwrap(), "correct horse")
            .await
            .expect_err("a corrupt payload must fail the export");

        assert!(
            error.contains("could not be fully decrypted"),
            "the error should say the backup is incomplete: {error}"
        );
        assert!(
            error.contains("unreadable clip payload"),
            "the error should name the payload field type: {error}"
        );
        assert!(
            !error.contains("sbs-772-payload-secret"),
            "the error must not expose clipboard contents: {error}"
        );
        assert!(
            !path.exists(),
            "a refused export must not create the destination file"
        );
        assert!(
            leftover_backup_temps(path.parent().unwrap()).is_empty(),
            "a refused export must not leave temporary output"
        );
    }

    /// SBS-772: optional rich-format fields are still history. Omitting them
    /// and succeeding would let the user believe the backup is complete.
    #[tokio::test]
    async fn export_refuses_a_corrupt_optional_rich_format_field() {
        let source = test_database().await;
        insert_rich_clip(&source, "sbs-772-rich-secret").await;
        corrupt_text_column(&source, "sbs-772-rich-secret", "metadata").await;

        let path = temp_path("corrupt-rich");
        let error = export_backup(&source, path.to_str().unwrap(), "correct horse")
            .await
            .expect_err("a corrupt rich-format field must fail the export");

        assert!(
            error.contains("could not be fully decrypted"),
            "the error should say the backup is incomplete: {error}"
        );
        assert!(
            error.contains("unreadable rich format"),
            "the error should name the rich-format field type: {error}"
        );
        assert!(
            !error.contains("sbs-772-rich-secret") && !error.contains("<html>rich</html>"),
            "the error must not expose clipboard contents: {error}"
        );
        assert!(
            !path.exists(),
            "a refused export must not create the destination file"
        );
    }

    /// SBS-772: notes, recognized text, and preview are clip fields too.
    /// Defaulting or dropping them is the same silent omission as skipping a row.
    #[tokio::test]
    async fn export_refuses_corrupt_notes_preview_and_recognized_text() {
        for (column, expected_label, marker) in [
            ("notes", "unreadable notes", "sbs-772-notes-secret"),
            (
                "text_preview",
                "unreadable preview",
                "sbs-772-preview-secret",
            ),
            (
                "ocr_text",
                "unreadable recognized text",
                "sbs-772-ocr-secret",
            ),
        ] {
            let source = test_database().await;
            insert_rich_clip(&source, marker).await;
            corrupt_text_column(&source, marker, column).await;

            let path = temp_path(&format!("corrupt-{column}"));
            let error = export_backup(&source, path.to_str().unwrap(), "correct horse")
                .await
                .expect_err(&format!("{column} must fail the export"));

            assert!(
                error.contains(expected_label),
                "{column} error should name the field type: {error}"
            );
            assert!(
                !error.contains(marker),
                "{column} error must not expose clipboard contents: {error}"
            );
            assert!(
                !path.exists(),
                "{column} refusal must not create the destination"
            );
        }
    }

    /// SBS-772: a failed export must not overwrite a file the user already has,
    /// and must not leave a sibling temp file behind.
    #[tokio::test]
    async fn failed_export_preserves_an_existing_destination_and_removes_temp_output() {
        let source = test_database().await;
        insert_rich_clip(&source, "sbs-772-keep-dest").await;
        corrupt_text_column(&source, "sbs-772-keep-dest", "notes").await;

        let path = temp_path("preserve-dest");
        let original = b"pre-existing destination must survive";
        std::fs::write(&path, original).unwrap();

        let error = export_backup(&source, path.to_str().unwrap(), "correct horse")
            .await
            .expect_err("a corrupt notes field must fail the export");
        assert!(error.contains("destination file was left unchanged"));
        assert_eq!(
            std::fs::read(&path).unwrap(),
            original,
            "the existing destination must be byte-for-byte unchanged"
        );
        assert!(
            leftover_backup_temps(path.parent().unwrap()).is_empty(),
            "failure must clean up temporary output"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// Persist writes a sibling temp file first; if the destination cannot be
    /// replaced, that temp file must not remain beside it.
    #[tokio::test]
    async fn persist_cleans_up_temp_output_when_the_destination_cannot_be_replaced() {
        let dir = std::env::temp_dir().join(format!("cubby-backup-dir-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("not-a-file");
        std::fs::create_dir(&dest).unwrap();

        let error = persist_backup_file(dest.to_str().unwrap(), b"complete-bundle-bytes")
            .expect_err("replacing a directory with a file must fail");
        assert!(error.contains("Could not save the backup"));
        assert!(
            leftover_backup_temps(&dir).is_empty(),
            "a failed persist must remove its temporary file"
        );
        assert!(dest.is_dir(), "the existing destination must remain");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// SBS-772: an unreadable field blocks the whole backup, but the clip list
    /// still shows every clip, so a message that only counts field types tells
    /// the user nothing they can act on. Name the row.
    #[tokio::test]
    async fn export_names_the_unreadable_clip_and_not_its_neighbor() {
        let source = test_database().await;
        insert_rich_clip(&source, "sbs-772-named-bad").await;
        insert_rich_clip(&source, "sbs-772-named-good").await;
        corrupt_text_column(&source, "sbs-772-named-bad", "notes").await;

        let bad = uuid_of(&source, "sbs-772-named-bad").await;
        let good = uuid_of(&source, "sbs-772-named-good").await;

        let path = temp_path("named-clip");
        let error = export_backup(&source, path.to_str().unwrap(), "correct horse")
            .await
            .expect_err("a corrupt note must fail the export");

        assert!(
            error.contains(&bad),
            "the error should name the unreadable clip {bad}: {error}"
        );
        assert!(
            !error.contains(&good),
            "the error must not name the readable neighbor {good}: {error}"
        );
        assert!(
            !error.contains("sbs-772-named-bad"),
            "the error must not expose clipboard contents: {error}"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A storage key that stopped working fails every row. Listing thousands of
    /// ids in a toast helps nobody, so the list is capped and the rest counted.
    #[tokio::test]
    async fn export_caps_how_many_clip_ids_it_names() {
        let source = test_database().await;
        let total = MAX_REPORTED_FAILED_CLIPS + 3;
        for index in 0..total {
            let text = format!("sbs-772-mass-{index}");
            insert_rich_clip(&source, &text).await;
            corrupt_text_column(&source, &text, "notes").await;
        }

        let path = temp_path("mass-corruption");
        let error = export_backup(&source, path.to_str().unwrap(), "correct horse")
            .await
            .expect_err("every clip being unreadable must fail the export");

        let mut named = 0;
        for index in 0..total {
            if error.contains(&uuid_of(&source, &format!("sbs-772-mass-{index}")).await) {
                named += 1;
            }
        }
        assert_eq!(
            named, MAX_REPORTED_FAILED_CLIPS,
            "only the capped number of ids should appear: {error}"
        );
        assert!(
            error.contains("and 3 more"),
            "the rest should be counted, not listed: {error}"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// SBS-919: a live screenshot original must survive a PC-migration
    /// backup. The thumbnail in `clips.content` is not a substitute.
    #[tokio::test]
    async fn full_resolution_image_survives_a_round_trip() {
        let source = test_database().await;
        let thumbnail = b"thumb-320x220-not-the-original";
        let original = b"full-res-png-bytes-sbs-919-secret";
        insert_image_clip(&source, thumbnail, Some(original), false).await;

        let path = temp_path("full-res-roundtrip");
        let exported = export_backup(&source, path.to_str().unwrap(), "correct horse")
            .await
            .expect("a live original must export");
        assert_eq!(exported, 1);

        let raw = std::fs::read(&path).unwrap();
        assert!(
            !raw.windows(original.len()).any(|window| window == original),
            "the bundle on disk must not hold the original in the clear"
        );

        let target = test_database().await;
        let result = import_backup(&target, path.to_str().unwrap(), "correct horse", false)
            .await
            .expect("import should succeed");
        assert_eq!(result.imported, 1);
        assert!(result.errors.is_empty());

        let (expired, restored, restored_thumb) = restored_image_state(&target).await;
        assert!(
            !expired,
            "a restored original must not be marked full_image_expired"
        );
        assert_eq!(
            restored.as_deref(),
            Some(original.as_slice()),
            "the restored file must be the full-resolution original"
        );
        assert_eq!(
            restored_thumb, thumbnail,
            "clips.content must stay the thumbnail, not the original"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let _ = std::fs::remove_dir_all(&target.image_dir);
        let _ = std::fs::remove_dir_all(&source.image_dir);
    }

    /// A restore of a screenshot older than dest keep-for must not age-sweep
    /// the original on the next retention pass. created_at stays the source
    /// date; last_accessed is the dest keep-for clock.
    #[tokio::test]
    async fn restored_live_original_survives_dest_keep_for_window() {
        let source = test_database().await;
        let original = b"full-res-png-bytes-sbs-919-keep-for";
        insert_image_clip(&source, b"thumb-keep-for", Some(original), false).await;

        let path = temp_path("keep-for-restore");
        export_backup(&source, path.to_str().unwrap(), "correct horse")
            .await
            .unwrap();

        let target = test_database().await;
        import_backup(&target, path.to_str().unwrap(), "correct horse", false)
            .await
            .unwrap();

        crate::commands::enforce_retention_in_pool(&target.pool, 0, 30)
            .await
            .expect("retention should run");

        let (expired, restored, _) = restored_image_state(&target).await;
        assert!(
            !expired,
            "a just-imported live original must stay full_image_expired=0"
        );
        assert_eq!(
            restored.as_deref(),
            Some(original.as_slice()),
            "the .cubby original must still exist after dest 30-day retention"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let _ = std::fs::remove_dir_all(&target.image_dir);
        let _ = std::fs::remove_dir_all(&source.image_dir);
    }

    /// Retention can expire a live original after export has already snapshotted
    /// full_image_expired=0. Re-read the flag and continue as thumbnail-only
    /// instead of failing the whole backup.
    #[tokio::test]
    async fn export_treats_a_now_expired_original_as_thumbnail_only() {
        let source = test_database().await;
        let uuid = insert_image_clip(
            &source,
            b"stale-expired-thumb",
            Some(b"live-original"),
            false,
        )
        .await;
        sqlx::query("UPDATE clips SET full_image_expired = 1 WHERE uuid = ?")
            .bind(&uuid)
            .execute(&source.pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM clip_images WHERE clip_uuid = ?")
            .bind(&uuid)
            .execute(&source.pool)
            .await
            .unwrap();
        let file = source.image_dir.join(format!("{uuid}.cubby"));
        let _ = std::fs::remove_file(&file);

        let clip = BackupClip {
            clip_type: "image".to_string(),
            content_b64: BASE64.encode(b"stale-expired-thumb"),
            text_preview: "[Image]".to_string(),
            is_pinned: false,
            is_hidden: false,
            notes: None,
            source_app: None,
            source_icon: None,
            metadata: None,
            ocr_text: None,
            created_at: "2026-05-01 09:00:00".to_string(),
            folder: None,
            full_image_b64: None,
        };
        let exported = attach_export_full_image(&source, &uuid, clip, false)
            .await
            .expect("a now-expired original must not fail the export");
        assert!(
            exported.full_image_b64.is_none(),
            "the bundle should stay thumbnail-only"
        );
        let _ = std::fs::remove_dir_all(&source.image_dir);
    }

    /// Decrypting a legacy blob to empty is not a valid original. Export must
    /// refuse rather than write a bundle import would silently skip.
    #[tokio::test]
    async fn export_refuses_a_legacy_original_that_decrypts_empty() {
        let source = test_database().await;
        let uuid =
            insert_image_clip(&source, b"empty-decrypt-thumb", Some(b"not-empty"), false).await;
        let file = source.image_dir.join(format!("{uuid}.cubby"));
        let _ = std::fs::remove_file(&file);
        sqlx::query("UPDATE clip_images SET full_content = ?, file_path = '' WHERE clip_uuid = ?")
            .bind(source.crypto.encrypt(&[]).unwrap())
            .bind(&uuid)
            .execute(&source.pool)
            .await
            .unwrap();

        let path = temp_path("empty-decrypt-original");
        export_backup(&source, path.to_str().unwrap(), "correct horse")
            .await
            .expect_err("an empty decrypted original must fail the export");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let _ = std::fs::remove_dir_all(&source.image_dir);
    }

    /// Index insert can fail while the clipboard loop holds the database.
    /// Delete the clips row first so a leftover live-looking row cannot make a
    /// retry look like a duplicate of a thumbnail.
    #[tokio::test]
    async fn index_failure_does_not_leave_a_clip_that_blocks_retry() {
        let source = test_database().await;
        insert_image_clip(
            &source,
            b"index-fail-thumb",
            Some(b"index-fail-original"),
            false,
        )
        .await;
        let path = temp_path("index-fail-retry");
        export_backup(&source, path.to_str().unwrap(), "correct horse")
            .await
            .unwrap();

        let target = test_database().await;
        sqlx::query(
            "CREATE TRIGGER fail_index BEFORE INSERT ON clip_images BEGIN SELECT RAISE(ABORT, 'injected'); END",
        )
        .execute(&target.pool)
        .await
        .unwrap();

        let failed = import_backup(&target, path.to_str().unwrap(), "correct horse", false)
            .await
            .unwrap();
        assert_eq!(
            failed.imported, 0,
            "the failed index must not count as imported"
        );
        let leftover: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM clips")
            .fetch_one(&target.pool)
            .await
            .unwrap();
        assert_eq!(
            leftover, 0,
            "the clips row must be gone so a retry can restore"
        );

        sqlx::query("DROP TRIGGER fail_index")
            .execute(&target.pool)
            .await
            .unwrap();
        let retry = import_backup(&target, path.to_str().unwrap(), "correct horse", false)
            .await
            .unwrap();
        assert_eq!(
            retry.imported, 1,
            "retry must not be treated as a duplicate"
        );
        assert_eq!(retry.duplicates, 0);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let _ = std::fs::remove_dir_all(&target.image_dir);
        let _ = std::fs::remove_dir_all(&source.image_dir);
    }

    /// SBS-919: retention already dropped the original. That is a first-class
    /// state, not a missing file, so export must succeed without one.
    #[tokio::test]
    async fn expired_image_exports_without_an_original() {
        let source = test_database().await;
        insert_image_clip(&source, b"expired-thumb", None, true).await;

        let path = temp_path("expired-image");
        export_backup(&source, path.to_str().unwrap(), "correct horse")
            .await
            .expect("an already-expired image must still export");

        let target = test_database().await;
        import_backup(&target, path.to_str().unwrap(), "correct horse", false)
            .await
            .unwrap();
        let (expired, original, thumb) = restored_image_state(&target).await;
        assert!(expired, "an expired image must stay expired after import");
        assert!(original.is_none(), "no original should have been written");
        assert_eq!(thumb, b"expired-thumb");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// SBS-919: only an image clip may carry a full-resolution original. A
    /// crafted bundle that pairs `full_image_b64` with a text clip must not get
    /// a PNG written into the image directory or a `clip_images` row the UI
    /// never expects for text.
    #[tokio::test]
    async fn import_ignores_a_full_image_on_a_non_image_clip() {
        let bundle = BackupBundle {
            version: 1,
            exported_at: "2026-05-01 09:00:00".to_string(),
            clips: vec![BackupClip {
                clip_type: "text".to_string(),
                content_b64: BASE64.encode(b"just some text"),
                text_preview: "just some text".to_string(),
                is_pinned: false,
                is_hidden: false,
                notes: None,
                source_app: None,
                source_icon: None,
                metadata: None,
                ocr_text: None,
                created_at: "2026-05-01 09:00:00".to_string(),
                folder: None,
                full_image_b64: Some(BASE64.encode(b"not really a screenshot")),
            }],
        };
        let path = temp_path("full-image-on-text");
        std::fs::write(
            &path,
            seal_bundle(&bundle, "correct horse").expect("the crafted bundle should seal"),
        )
        .unwrap();

        let target = test_database().await;
        let result = import_backup(&target, path.to_str().unwrap(), "correct horse", false)
            .await
            .expect("a text clip with a stray full image must still import");
        assert_eq!(result.imported, 1, "the text clip itself must import");
        assert!(
            result.errors.is_empty(),
            "the stray original is dropped, not reported: {:?}",
            result.errors
        );

        let indexed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM clip_images")
            .fetch_one(&target.pool)
            .await
            .unwrap();
        assert_eq!(indexed, 0, "a text clip must not get a clip_images row");
        let written = std::fs::read_dir(&target.image_dir)
            .map(|entries| entries.count())
            .unwrap_or(0);
        assert_eq!(written, 0, "no original file should have been written");

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let _ = std::fs::remove_dir_all(&target.image_dir);
    }

    /// SBS-919: a bundle written before this field existed still imports, as
    /// the thumbnail-only expired state that version actually produced.
    #[test]
    fn old_bundle_clip_without_full_image_field_deserializes() {
        let json = r#"{
            "clip_type": "image",
            "content_b64": "dGh1bWI=",
            "text_preview": "[Image]",
            "is_pinned": false,
            "created_at": "2026-05-01 09:00:00"
        }"#;
        let clip: BackupClip = serde_json::from_str(json).expect("pre-SBS-919 clip must parse");
        assert_eq!(clip.clip_type, "image");
        assert!(
            clip.full_image_b64.is_none(),
            "a missing field must default to no original"
        );
    }

    /// SBS-919: a live image whose original cannot be read must refuse the
    /// export rather than write a bundle that looks complete and then expire
    /// every screenshot on restore.
    #[tokio::test]
    async fn export_refuses_a_missing_full_resolution_original() {
        let source = test_database().await;
        let uuid =
            insert_image_clip(&source, b"thumb-only-because-file-is-gone", None, false).await;

        let path = temp_path("missing-original");
        let original_dest = b"pre-existing destination must survive";
        std::fs::write(&path, original_dest).unwrap();

        let error = export_backup(&source, path.to_str().unwrap(), "correct horse")
            .await
            .expect_err("a missing live original must fail the export");

        assert!(
            error.contains("unreadable full-resolution image"),
            "the error should name the full-image field type: {error}"
        );
        assert!(
            error.contains(&uuid),
            "the error should name the affected clip {uuid}: {error}"
        );
        assert!(
            !error.contains("thumb-only-because-file-is-gone"),
            "the error must not expose clip bytes: {error}"
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            original_dest,
            "a refused export must leave the destination unchanged"
        );
        assert!(
            leftover_backup_temps(path.parent().unwrap()).is_empty(),
            "a refused export must not leave temporary output"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// SBS-919: an original that is present but cannot be decrypted is not
    /// "nothing there". Fail closed instead of substituting the thumbnail.
    #[tokio::test]
    async fn export_refuses_an_unreadable_full_resolution_original() {
        let source = test_database().await;
        let uuid = insert_image_clip(
            &source,
            b"readable-thumb",
            Some(b"full-res-that-will-be-corrupted"),
            false,
        )
        .await;
        let file_path = source.image_dir.join(format!("{uuid}.cubby"));
        std::fs::write(&file_path, b"not-an-encrypted-original").unwrap();

        let path = temp_path("corrupt-original");
        let error = export_backup(&source, path.to_str().unwrap(), "correct horse")
            .await
            .expect_err("an unreadable original must fail the export");

        assert!(
            error.contains("unreadable full-resolution image"),
            "the error should name the full-image field type: {error}"
        );
        assert!(
            error.contains(&uuid),
            "the error should name the affected clip {uuid}: {error}"
        );
        assert!(
            !error.contains("full-res-that-will-be-corrupted"),
            "the error must not expose the original bytes: {error}"
        );
        assert!(
            !path.exists(),
            "a refused export must not create the destination file"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let _ = std::fs::remove_dir_all(&source.image_dir);
    }

    /// `MoveFileExW` refuses paths above `MAX_PATH` unless they are `\\?\`
    /// prefixed, while `std::fs::write` prefixes them itself. A save-dialog name
    /// this long is legal, so persisting to it must still work.
    #[test]
    fn persist_writes_a_destination_longer_than_the_legacy_path_limit() {
        let dir = std::env::temp_dir().join(format!("cubby-backup-long-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // 239 characters: legal as a file name (the limit is 255), long enough
        // that the full path clears MAX_PATH on any plausible temp directory.
        let dest = dir.join(format!("{}.cubbybak", "n".repeat(230)));
        assert!(
            dest.as_os_str().len() > 260,
            "the test destination must exceed MAX_PATH to be meaningful"
        );

        persist_backup_file(dest.to_str().unwrap(), b"complete-bundle-bytes")
            .expect("a long but legal destination must still be written");

        assert_eq!(std::fs::read(&dest).unwrap(), b"complete-bundle-bytes");
        assert!(
            leftover_backup_temps(&dir).is_empty(),
            "a successful persist must leave no temporary file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
