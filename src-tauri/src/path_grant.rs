//! Process-local grants that bind backup/import IPC paths to the file picker.
//!
//! SBS-808: `export_backup`, `import_backup`, and `import_from_ditto` used to
//! take a raw renderer-supplied path and pass it to `std::fs`. A compromised
//! Settings page could then write decrypted history (re-encrypted under an
//! attacker passphrase) to a UNC path. Same-user malware can already unwrap
//! `storage.key`, so this is defense-in-depth: the native command must not
//! accept a path the dialog did not produce.
//!
//! Grants are in-memory only, keyed by purpose, and compared as the exact
//! string the picker returned. The library functions in `backup` and
//! `ditto_import` stay callable from their own tests without a grant.

use crate::database::Database;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// Why a path was picked. A save grant must not authorize import, and a Ditto
/// pick must not authorize backup export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PathGrantPurpose {
    BackupSave,
    BackupOpen,
    DittoOpen,
}

const UNTRUSTED_PATH: &str = "That path was not selected in the file dialog";
const EMPTY_PATH: &str = "A file path is required";
const GRANT_TABLE_UNREADABLE: &str = "Could not verify the selected file path";

pub struct PathGrantTable {
    grants: Mutex<HashMap<PathGrantPurpose, String>>,
}

impl PathGrantTable {
    pub fn new() -> Self {
        Self {
            grants: Mutex::new(HashMap::new()),
        }
    }

    pub fn grant(&self, purpose: PathGrantPurpose, path: String) -> Result<String, String> {
        if path.is_empty() {
            return Err(EMPTY_PATH.to_string());
        }
        let mut grants = lock_grants(&self.grants)?;
        grants.insert(purpose, path.clone());
        Ok(path)
    }

    /// SBS-808: reject a renderer path the matching picker did not produce.
    ///
    /// Compare the IPC string to the grant as returned by the dialog. Do not
    /// canonicalize a renderer-supplied path into a grant, and do not accept a
    /// different string that happens to resolve to the same file.
    ///
    /// A dry-run (`consume == false`) leaves the grant in place so preview
    /// then confirm can reuse the same path. The first mutating attempt
    /// consumes the grant whether the later read or write succeeds or fails.
    pub fn authorize(
        &self,
        purpose: PathGrantPurpose,
        path: &str,
        consume: bool,
    ) -> Result<(), String> {
        if path.is_empty() {
            return Err(EMPTY_PATH.to_string());
        }
        let mut grants = lock_grants(&self.grants)?;
        match grants.get(&purpose) {
            Some(granted) if granted == path => {
                if consume {
                    grants.remove(&purpose);
                }
                Ok(())
            }
            _ => Err(UNTRUSTED_PATH.to_string()),
        }
    }
}

impl Default for PathGrantTable {
    fn default() -> Self {
        Self::new()
    }
}

fn lock_grants(
    grants: &Mutex<HashMap<PathGrantPurpose, String>>,
) -> Result<std::sync::MutexGuard<'_, HashMap<PathGrantPurpose, String>>, String> {
    // Poison and any other unreadable table are unknown, not empty. Fail closed.
    grants
        .lock()
        .map_err(|_| GRANT_TABLE_UNREADABLE.to_string())
}

static GRANTS: OnceLock<PathGrantTable> = OnceLock::new();

fn grants() -> &'static PathGrantTable {
    GRANTS.get_or_init(PathGrantTable::new)
}

pub fn grant_picker_path(purpose: PathGrantPurpose, path: String) -> Result<String, String> {
    grants().grant(purpose, path)
}

pub fn authorize_picker_path(
    purpose: PathGrantPurpose,
    path: &str,
    consume: bool,
) -> Result<(), String> {
    grants().authorize(purpose, path, consume)
}

/// IPC body for `export_backup`. The grant is checked before any write.
pub async fn export_granted_backup(
    db: &Database,
    path: String,
    passphrase: String,
) -> Result<usize, String> {
    authorize_picker_path(PathGrantPurpose::BackupSave, &path, true)?;
    crate::backup::export_backup(db, &path, &passphrase).await
}

/// IPC body for `import_backup`. Dry-run keeps the grant; a mutating call
/// consumes it before the file is read.
pub async fn import_granted_backup(
    db: &Database,
    path: String,
    passphrase: String,
    dry_run: bool,
) -> Result<crate::backup::BackupImportResult, String> {
    authorize_picker_path(PathGrantPurpose::BackupOpen, &path, !dry_run)?;
    crate::backup::import_backup(db, &path, &passphrase, dry_run).await
}

/// IPC body for `import_from_ditto`. Dry-run keeps the grant; a mutating call
/// consumes it before the Ditto database is copied.
pub async fn import_granted_ditto(
    db: &Database,
    db_path: String,
    dry_run: bool,
) -> Result<crate::ditto_import::DittoImportResult, String> {
    authorize_picker_path(PathGrantPurpose::DittoOpen, &db_path, !dry_run)?;
    crate::ditto_import::import_from_ditto(db, &db_path, dry_run).await
}

#[cfg(test)]
pub(crate) fn reset_grants_for_tests() {
    let mut grants = grants()
        .grants
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    grants.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::Database;

    async fn isolated_grants() -> tokio::sync::MutexGuard<'static, ()> {
        static LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        let guard = LOCK.lock().await;
        reset_grants_for_tests();
        guard
    }

    fn temp_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "cubby-grant-{label}-{}.cubbybak",
            uuid::Uuid::new_v4()
        ))
    }

    async fn test_database() -> Database {
        let database = Database {
            pool: sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect("sqlite::memory:")
                .await
                .expect("in-memory database should open"),
            crypto: std::sync::Arc::new(crate::crypto::CryptoManager::ephemeral()),
            image_dir: std::env::temp_dir().join(format!("cubby-grant-{}", uuid::Uuid::new_v4())),
            search_index: std::sync::Arc::new(crate::search_index::SearchIndex::default()),
        };
        database.migrate().await.expect("migration should succeed");
        database
    }

    async fn insert_text_clip(db: &Database, text: &str) {
        let material = crate::clipboard::build_clip_hash_material(
            "text",
            text.as_bytes(),
            std::iter::empty::<(&str, &[u8])>(),
        );
        sqlx::query(
            r#"INSERT INTO clips (uuid, clip_type, content, text_preview, content_hash, is_pinned, created_at, last_accessed)
               VALUES (?, 'text', ?, ?, ?, 0, '2026-05-01 09:00:00', '2026-05-01 09:00:00')"#,
        )
        .bind(uuid::Uuid::new_v4().to_string())
        .bind(db.crypto.encrypt(text.as_bytes()).unwrap())
        .bind(db.crypto.encrypt_text(text).unwrap())
        .bind(db.crypto.keyed_hash(&material))
        .execute(&db.pool)
        .await
        .unwrap();
    }

    async fn live_clip_count(db: &Database) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM clips WHERE is_deleted = 0")
            .fetch_one(&db.pool)
            .await
            .unwrap()
    }

    async fn write_library_backup(text: &str) -> (std::path::PathBuf, String) {
        let source = test_database().await;
        insert_text_clip(&source, text).await;
        let path = temp_path("library");
        let path_str = path.to_string_lossy().to_string();
        crate::backup::export_backup(&source, &path_str, "correct horse")
            .await
            .expect("library export does not need a grant");
        (path, path_str)
    }

    /// A renderer-supplied export path that was never picked must not create a file.
    #[tokio::test]
    async fn never_picked_path_is_rejected_for_export_backup() {
        let _lock = isolated_grants().await;
        let db = test_database().await;
        insert_text_clip(&db, "secret history").await;
        let path = temp_path("never-export");
        let path_str = path.to_string_lossy().to_string();

        let error = export_granted_backup(&db, path_str, "passphrase".into())
            .await
            .expect_err("export_backup must reject a never-picked path");
        assert_eq!(error, UNTRUSTED_PATH);
        assert!(
            !error.to_lowercase().contains("secret"),
            "the reject error must not include clipboard contents: {error}"
        );
        assert!(
            !path.exists(),
            "rejected export_backup must not create the destination"
        );
    }

    /// A renderer-supplied import path that was never picked must not restore clips.
    #[tokio::test]
    async fn never_picked_path_is_rejected_for_import_backup() {
        let _lock = isolated_grants().await;
        let (bundle, path_str) = write_library_backup("secret history").await;
        let target = test_database().await;

        let error = import_granted_backup(&target, path_str, "correct horse".into(), false)
            .await
            .expect_err("import_backup must reject a never-picked path");
        assert_eq!(error, UNTRUSTED_PATH);
        assert!(
            !error.to_lowercase().contains("secret"),
            "the reject error must not include clipboard contents: {error}"
        );
        assert_eq!(
            live_clip_count(&target).await,
            0,
            "rejected import_backup must not read the bundle into history"
        );
        let _ = std::fs::remove_file(bundle);
    }

    /// A renderer-supplied Ditto path that was never picked must not be copied.
    #[tokio::test]
    async fn never_picked_path_is_rejected_for_import_from_ditto() {
        let _lock = isolated_grants().await;
        let db = test_database().await;
        let path = std::env::temp_dir().join(format!(
            "cubby-grant-never-ditto-{}.db",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&path, b"not a ditto database").unwrap();
        let path_str = path.to_string_lossy().to_string();

        let error = import_granted_ditto(&db, path_str, false)
            .await
            .expect_err("import_from_ditto must reject a never-picked path");
        assert_eq!(error, UNTRUSTED_PATH);
        assert_eq!(
            live_clip_count(&db).await,
            0,
            "rejected import_from_ditto must not copy or import the file"
        );
        let _ = std::fs::remove_file(path);
    }

    /// A save grant must not authorize import, and an open grant must not authorize export.
    #[tokio::test]
    async fn wrong_purpose_grant_is_rejected() {
        let _lock = isolated_grants().await;
        let db = test_database().await;
        insert_text_clip(&db, "secret history").await;
        let (bundle, import_path) = write_library_backup("secret history").await;
        let export_path = temp_path("wrong-purpose");
        let export_str = export_path.to_string_lossy().to_string();

        grant_picker_path(PathGrantPurpose::BackupSave, import_path.clone()).unwrap();
        let import_error =
            import_granted_backup(&db, import_path.clone(), "correct horse".into(), false)
                .await
                .expect_err("a save grant must not authorize import_backup");
        assert_eq!(import_error, UNTRUSTED_PATH);
        assert_eq!(live_clip_count(&db).await, 1);

        reset_grants_for_tests();
        grant_picker_path(PathGrantPurpose::BackupOpen, export_str.clone()).unwrap();
        let export_error = export_granted_backup(&db, export_str.clone(), "passphrase".into())
            .await
            .expect_err("an open grant must not authorize export_backup");
        assert_eq!(export_error, UNTRUSTED_PATH);
        assert!(
            !export_path.exists(),
            "wrong-purpose export_backup must not create a file"
        );

        reset_grants_for_tests();
        grant_picker_path(PathGrantPurpose::DittoOpen, export_str.clone()).unwrap();
        let ditto_export_error = export_granted_backup(&db, export_str, "passphrase".into())
            .await
            .expect_err("a Ditto grant must not authorize export_backup");
        assert_eq!(ditto_export_error, UNTRUSTED_PATH);
        assert!(!export_path.exists());

        let _ = std::fs::remove_file(bundle);
    }

    /// Preview then confirm reuses the same granted path; dry-run must not consume it.
    #[tokio::test]
    async fn granted_path_survives_dry_run_then_is_consumed_by_mutating_import() {
        let _lock = isolated_grants().await;
        let (bundle, path_str) = write_library_backup("restored clip").await;
        grant_picker_path(PathGrantPurpose::BackupOpen, path_str.clone()).unwrap();

        let target = test_database().await;
        let preview =
            import_granted_backup(&target, path_str.clone(), "correct horse".into(), true)
                .await
                .expect("dry-run import_backup must accept a granted path");
        assert!(preview.dry_run);
        assert_eq!(preview.imported, 1);
        assert_eq!(live_clip_count(&target).await, 0);

        let imported =
            import_granted_backup(&target, path_str.clone(), "correct horse".into(), false)
                .await
                .expect("mutating import_backup must still accept the path after dry-run");
        assert!(!imported.dry_run);
        assert_eq!(imported.imported, 1);
        assert_eq!(live_clip_count(&target).await, 1);

        let reused = import_granted_backup(&target, path_str, "correct horse".into(), false)
            .await
            .expect_err("the grant must be consumed by the first mutating call");
        assert_eq!(reused, UNTRUSTED_PATH);

        let _ = std::fs::remove_file(bundle);
    }

    /// The first mutating export consumes the grant even when the write later fails.
    #[tokio::test]
    async fn mutating_export_consumes_the_grant() {
        let _lock = isolated_grants().await;
        let db = test_database().await;
        insert_text_clip(&db, "exported clip").await;
        let path = temp_path("consume-export");
        let path_str = path.to_string_lossy().to_string();
        grant_picker_path(PathGrantPurpose::BackupSave, path_str.clone()).unwrap();

        let count = export_granted_backup(&db, path_str.clone(), "correct horse".into())
            .await
            .expect("export_backup must accept a granted save path");
        assert_eq!(count, 1);
        assert!(path.exists());

        let reused = export_granted_backup(&db, path_str, "passphrase".into())
            .await
            .expect_err("the save grant must be consumed after the mutating export");
        assert_eq!(reused, UNTRUSTED_PATH);

        let _ = std::fs::remove_file(path);
    }

    /// A mutating attempt consumes the grant even if the later filesystem call fails.
    #[tokio::test]
    async fn mutating_attempt_consumes_grant_on_failure() {
        let _lock = isolated_grants().await;
        let db = test_database().await;
        let path = temp_path("consume-on-fail");
        let path_str = path.to_string_lossy().to_string();
        grant_picker_path(PathGrantPurpose::BackupSave, path_str.clone()).unwrap();

        let first = export_granted_backup(&db, path_str.clone(), String::new())
            .await
            .expect_err("an empty passphrase should fail after the grant is consumed");
        assert_ne!(first, UNTRUSTED_PATH);
        assert!(!path.exists());

        let second = export_granted_backup(&db, path_str, "passphrase".into())
            .await
            .expect_err("a failed mutating call must still consume the grant");
        assert_eq!(second, UNTRUSTED_PATH);
    }

    /// An empty path is unknown, not a picker result.
    #[tokio::test]
    async fn empty_path_is_rejected() {
        let _lock = isolated_grants().await;
        let db = test_database().await;
        grant_picker_path(
            PathGrantPurpose::BackupSave,
            "C:\\picked\\backup.cubbybak".into(),
        )
        .unwrap();
        grant_picker_path(
            PathGrantPurpose::BackupOpen,
            "C:\\picked\\backup.cubbybak".into(),
        )
        .unwrap();
        grant_picker_path(PathGrantPurpose::DittoOpen, "C:\\picked\\Ditto.db".into()).unwrap();

        let export = export_granted_backup(&db, String::new(), "passphrase".into())
            .await
            .expect_err("export_backup must reject an empty path");
        let import = import_granted_backup(&db, String::new(), "passphrase".into(), false)
            .await
            .expect_err("import_backup must reject an empty path");
        let ditto = import_granted_ditto(&db, String::new(), false)
            .await
            .expect_err("import_from_ditto must reject an empty path");
        assert_eq!(export, EMPTY_PATH);
        assert_eq!(import, EMPTY_PATH);
        assert_eq!(ditto, EMPTY_PATH);
        assert_eq!(
            grant_picker_path(PathGrantPurpose::BackupSave, String::new())
                .expect_err("an empty picker result must not become a grant"),
            EMPTY_PATH
        );
    }

    /// A different string is not the grant, even if it looks like the same file.
    #[tokio::test]
    async fn lookalike_path_is_rejected_without_canonicalizing() {
        let _lock = isolated_grants().await;
        let db = test_database().await;
        insert_text_clip(&db, "secret history").await;
        let granted = "C:\\Users\\me\\backup.cubbybak".to_string();
        grant_picker_path(PathGrantPurpose::BackupSave, granted.clone()).unwrap();

        for lookalike in [
            "C:\\Users\\me\\backup.cubbybak.exe",
            "C:/Users/me/backup.cubbybak",
            "\\\\localhost\\C$\\Users\\me\\backup.cubbybak",
            "C:\\\\Users\\\\me\\\\backup.cubbybak",
            "C:\\Users\\me\\backup.cubbybak ",
        ] {
            let error = export_granted_backup(&db, lookalike.to_string(), "passphrase".into())
                .await
                .expect_err("a lookalike string must not satisfy the grant");
            assert_eq!(error, UNTRUSTED_PATH, "rejected {lookalike}");
        }

        // A rejected lookalike must not consume the real grant.
        let granted_error = export_granted_backup(&db, granted, String::new())
            .await
            .expect_err("the real grant should still be present and then fail on passphrase");
        assert_ne!(granted_error, UNTRUSTED_PATH);
    }

    /// Purpose is part of the grant even when the path strings match.
    #[test]
    fn table_rejects_wrong_purpose_for_the_same_path() {
        let table = PathGrantTable::new();
        table
            .grant(
                PathGrantPurpose::BackupSave,
                "C:\\Users\\me\\backup.cubbybak".into(),
            )
            .unwrap();
        let error = table
            .authorize(
                PathGrantPurpose::BackupOpen,
                "C:\\Users\\me\\backup.cubbybak",
                true,
            )
            .expect_err("the same string under another purpose is not a grant");
        assert_eq!(error, UNTRUSTED_PATH);
        table
            .authorize(
                PathGrantPurpose::BackupSave,
                "C:\\Users\\me\\backup.cubbybak",
                false,
            )
            .expect("wrong-purpose authorize must not consume the matching grant");
    }

    /// Mutex poison is unknown, not an empty table.
    #[test]
    fn poisoned_grant_table_is_rejected() {
        let table = PathGrantTable::new();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = table.grants.lock().unwrap();
            panic!("poison the grant table");
        }));
        let error = table
            .authorize(
                PathGrantPurpose::BackupSave,
                "C:\\picked\\backup.cubbybak",
                true,
            )
            .expect_err("a poisoned table must fail closed");
        assert_eq!(error, GRANT_TABLE_UNREADABLE);
        let grant_error = table
            .grant(
                PathGrantPurpose::BackupSave,
                "C:\\picked\\backup.cubbybak".into(),
            )
            .expect_err("granting into a poisoned table must fail closed");
        assert_eq!(grant_error, GRANT_TABLE_UNREADABLE);
    }
}
