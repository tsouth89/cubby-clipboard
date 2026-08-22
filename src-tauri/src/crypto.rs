use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::io;
use std::path::Path;

const ENVELOPE_MAGIC: &[u8; 4] = b"CUB1";
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;
const TEXT_PREFIX: &str = "CUB1:";

/// Why Cubby could not open its encrypted storage.
///
/// Everything except a DPAPI account mismatch collapses into `Other`, which
/// carries the same text the callers used to produce. The mismatch is separated
/// because it is the one failure with a real recovery path, and the one a user
/// can reach without anything being broken: DPAPI ties the storage key to the
/// Windows account that created it, so carrying a portable folder to another
/// user or PC produces exactly this and nothing else.
#[derive(Debug)]
pub enum StorageError {
    /// The key file is present and intact, but this Windows account cannot
    /// unprotect it. The key and database are left untouched, so the original
    /// account can still read them.
    KeyNotForThisUser {
        key_path: std::path::PathBuf,
        detail: String,
    },
    Other(String),
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::KeyNotForThisUser { key_path, detail } => write!(
                formatter,
                "the storage key at {} belongs to a different Windows account: {detail}",
                key_path.display()
            ),
            Self::Other(message) => formatter.write_str(message),
        }
    }
}

impl From<String> for StorageError {
    fn from(message: String) -> Self {
        Self::Other(message)
    }
}

fn load_protected_key(key_path: &Path) -> Result<[u8; KEY_LEN], StorageError> {
    let protected = std::fs::read(key_path)
        .map_err(|e| format!("failed to read protected storage key: {e}"))?;
    // A failure here is an account mismatch, not corruption: the bytes were
    // read fine and DPAPI refused them. Reported separately so startup can
    // explain it instead of dying, and so nothing treats it as a reason to
    // recreate the key over a database only the other account can read.
    let plaintext = unprotect_for_current_user(&protected).map_err(|detail| {
        StorageError::KeyNotForThisUser {
            key_path: key_path.to_path_buf(),
            detail,
        }
    })?;
    plaintext
        .try_into()
        .map_err(|_| StorageError::Other("protected storage key has an invalid length".to_string()))
}

#[derive(Clone)]
pub struct CryptoManager {
    key: [u8; KEY_LEN],
}

impl CryptoManager {
    pub fn load_or_create(db_path: &Path, allow_create: bool) -> Result<Self, StorageError> {
        let key_path = db_path.with_file_name("storage.key");
        let key = match key_path.try_exists() {
            Ok(true) => load_protected_key(&key_path)?,
            Err(error) => {
                return Err(StorageError::Other(format!(
                    "failed to inspect protected storage key path {}: {error}",
                    key_path.display()
                )));
            }
            Ok(false) => {
                if !allow_create {
                    return Err(StorageError::Other(
                        "encrypted clipboard history exists, but its protected storage key is missing"
                            .to_string(),
                    ));
                }
                let mut key = [0_u8; KEY_LEN];
                getrandom::fill(&mut key)
                    .map_err(|e| format!("failed to generate storage key: {e}"))?;
                let protected = protect_for_current_user(&key)?;
                if let Some(parent) = key_path.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("failed to create storage directory: {e}"))?;
                }
                let temporary_path = key_path.with_file_name(format!(
                    "storage.key.{}.{}.tmp",
                    std::process::id(),
                    uuid::Uuid::new_v4()
                ));
                std::fs::write(&temporary_path, protected)
                    .map_err(|e| format!("failed to persist protected storage key: {e}"))?;
                // CreateHardLinkW is NTFS-only. Portable first-run on FAT/exFAT
                // (the path our own tests name `D:\USB\Cubby\data`) used to
                // write a temp key, fail the link, delete the temp, and panic in
                // Database::new (SBS-908). Backup export already special-cases
                // FAT; this path has to as well.
                match install_storage_key_file(&temporary_path, &key_path)? {
                    KeyInstall::Installed => key,
                    KeyInstall::AlreadyPresent => load_protected_key(&key_path)?,
                }
            }
        };

        Ok(Self { key })
    }

    #[cfg(test)]
    pub fn ephemeral() -> Self {
        let mut key = [0_u8; KEY_LEN];
        getrandom::fill(&mut key).expect("test encryption key should be generated");
        Self { key }
    }

    pub fn is_encrypted(&self, value: &[u8]) -> bool {
        self.decrypt(value).is_ok()
    }

    pub fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>, String> {
        let cipher = Aes256Gcm::new_from_slice(&self.key)
            .map_err(|_| "failed to initialize storage encryption".to_string())?;
        let mut nonce = [0_u8; NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(|e| format!("failed to generate nonce: {e}"))?;
        let ciphertext = cipher
            .encrypt(Nonce::from_slice(&nonce), plaintext)
            .map_err(|_| "failed to encrypt clipboard content".to_string())?;

        let mut envelope = Vec::with_capacity(ENVELOPE_MAGIC.len() + NONCE_LEN + ciphertext.len());
        envelope.extend_from_slice(ENVELOPE_MAGIC);
        envelope.extend_from_slice(&nonce);
        envelope.extend_from_slice(&ciphertext);
        Ok(envelope)
    }

    pub fn decrypt(&self, envelope: &[u8]) -> Result<Vec<u8>, String> {
        if !envelope.starts_with(ENVELOPE_MAGIC) {
            return Err("clipboard payload is not encrypted".to_string());
        }
        if envelope.len() < ENVELOPE_MAGIC.len() + NONCE_LEN + 16 {
            return Err("encrypted clipboard payload is truncated".to_string());
        }
        let nonce_start = ENVELOPE_MAGIC.len();
        let ciphertext_start = nonce_start + NONCE_LEN;
        let cipher = Aes256Gcm::new_from_slice(&self.key)
            .map_err(|_| "failed to initialize storage encryption".to_string())?;
        cipher
            .decrypt(
                Nonce::from_slice(&envelope[nonce_start..ciphertext_start]),
                &envelope[ciphertext_start..],
            )
            .map_err(|_| "clipboard content failed authentication".to_string())
    }

    pub fn keyed_hash(&self, content: &[u8]) -> String {
        let mut mac =
            <Hmac<Sha256> as Mac>::new_from_slice(&self.key).expect("HMAC accepts a 256-bit key");
        mac.update(content);
        mac.finalize()
            .into_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    pub fn encrypt_text(&self, plaintext: &str) -> Result<String, String> {
        Ok(format!(
            "{TEXT_PREFIX}{}",
            BASE64.encode(self.encrypt(plaintext.as_bytes())?)
        ))
    }

    pub fn decrypt_text(&self, value: &str) -> Result<String, String> {
        if !value.starts_with(TEXT_PREFIX) {
            return Err("clipboard text field is not encrypted".to_string());
        }
        let envelope = BASE64
            .decode(&value[TEXT_PREFIX.len()..])
            .map_err(|_| "encrypted clipboard text is invalid".to_string())?;
        String::from_utf8(self.decrypt(&envelope)?)
            .map_err(|_| "decrypted clipboard text is not UTF-8".to_string())
    }

    pub fn is_encrypted_text(&self, value: &str) -> bool {
        self.decrypt_text(value).is_ok()
    }

    /// Prefix check only. A `CUB1:` blob that fails to decrypt is still an
    /// envelope: "unreadable" is not "legacy plaintext".
    pub(crate) fn is_text_envelope(value: &str) -> bool {
        value.starts_with(TEXT_PREFIX)
    }

    pub fn encrypt_optional_text(&self, value: Option<&str>) -> Result<Option<String>, String> {
        value.map(|value| self.encrypt_text(value)).transpose()
    }

    pub fn decrypt_optional_text(&self, value: &mut Option<String>) -> Result<(), String> {
        if let Some(ciphertext) = value {
            *ciphertext = self.decrypt_text(ciphertext)?;
        }
        Ok(())
    }
}

/// How the volume under `storage.key` answered "do you support hard links?"
///
/// Three states, not two. "GetVolumeInformationW failed" is not "this is
/// FAT", and collapsing them is how a lock or permission error would be
/// treated as "just rename it".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VolumeHardLinkSupport {
    Supported,
    Unsupported,
    Unknown,
}

enum KeyInstall {
    Installed,
    AlreadyPresent,
}

fn install_storage_key_file(
    temporary_path: &Path,
    key_path: &Path,
) -> Result<KeyInstall, StorageError> {
    install_storage_key_file_with(
        temporary_path,
        key_path,
        probe_volume_hard_link_support(temporary_path),
        |source, destination| std::fs::hard_link(source, destination),
    )
}

fn install_storage_key_file_with(
    temporary_path: &Path,
    key_path: &Path,
    volume: VolumeHardLinkSupport,
    hard_link: impl Fn(&Path, &Path) -> io::Result<()>,
) -> Result<KeyInstall, StorageError> {
    install_storage_key_file_using(
        temporary_path,
        key_path,
        volume,
        hard_link,
        key_is_already_present,
    )
}

/// `key_present` is injected so a test can fail the presence check the way a
/// locked or permission-denied parent does. That check is a `?` in the middle
/// of the install, so it is the path most likely to skip cleanup.
fn install_storage_key_file_using(
    temporary_path: &Path,
    key_path: &Path,
    volume: VolumeHardLinkSupport,
    hard_link: impl Fn(&Path, &Path) -> io::Result<()>,
    key_present: impl Fn(&Path) -> Result<bool, StorageError>,
) -> Result<KeyInstall, StorageError> {
    let outcome: Result<KeyInstall, StorageError> = (|| {
        let link_result = match volume {
            // Known no-hard-link volume (FAT/exFAT). Do not call
            // CreateHardLinkW; it cannot succeed, and the failure is not a race.
            VolumeHardLinkSupport::Unsupported => None,
            VolumeHardLinkSupport::Supported | VolumeHardLinkSupport::Unknown => {
                Some(hard_link(temporary_path, key_path))
            }
        };

        match link_result {
            Some(Ok(())) => return Ok(KeyInstall::Installed),
            Some(Err(_)) if key_present(key_path)? => return Ok(KeyInstall::AlreadyPresent),
            Some(Err(error)) if hard_link_error_means_unsupported(&error) => {
                // Volume probe said Supported or Unknown, but the link error is
                // the specific "this filesystem cannot do this" set. Fall
                // through to an exclusive rename. A lock or access-denied does
                // not land here.
            }
            Some(Err(error)) => return Err(install_key_error(volume, error)),
            None => {}
        }

        match rename_without_replace(temporary_path, key_path) {
            Ok(()) => Ok(KeyInstall::Installed),
            Err(_) if key_present(key_path)? => Ok(KeyInstall::AlreadyPresent),
            Err(error) => Err(StorageError::Other(format!(
                "failed to install protected storage key: {error}"
            ))),
        }
    })();

    // One cleanup for every exit, including the `?` on the presence checks
    // above. Leaving the temp behind leaves generated key bytes on disk, and a
    // first run that keeps failing keeps accumulating them. After a successful
    // rename the path is already gone, so this is a no-op there.
    let _ = std::fs::remove_file(temporary_path);
    outcome
}

fn install_key_error(volume: VolumeHardLinkSupport, error: io::Error) -> StorageError {
    let message = match volume {
        VolumeHardLinkSupport::Unknown => format!(
            "failed to install protected storage key: {error} (volume hard-link support could not be determined)"
        ),
        _ => format!("failed to install protected storage key: {error}"),
    };
    StorageError::Other(message)
}

/// `exists()` treats an unreadable path as missing. That is the three-state
/// collapse: a permission or lock error is not "no key yet".
fn key_is_already_present(key_path: &Path) -> Result<bool, StorageError> {
    key_path.try_exists().map_err(|error| {
        StorageError::Other(format!(
            "failed to inspect protected storage key path {}: {error}",
            key_path.display()
        ))
    })
}

fn hard_link_error_means_unsupported(error: &io::Error) -> bool {
    #[cfg(target_os = "windows")]
    {
        // CreateHardLinkW on FAT/exFAT: ERROR_INVALID_FUNCTION (1).
        // Some filter drivers surface ERROR_NOT_SUPPORTED (50).
        matches!(error.raw_os_error(), Some(1) | Some(50))
    }
    #[cfg(not(target_os = "windows"))]
    {
        error.kind() == io::ErrorKind::Unsupported
    }
}

fn probe_volume_hard_link_support(path: &Path) -> VolumeHardLinkSupport {
    match volume_reports_hard_links(path) {
        Ok(true) => VolumeHardLinkSupport::Supported,
        Ok(false) => VolumeHardLinkSupport::Unsupported,
        Err(_) => VolumeHardLinkSupport::Unknown,
    }
}

/// `Ok(true)` / `Ok(false)` are answers. `Err` is "we could not ask".
#[cfg(target_os = "windows")]
fn volume_reports_hard_links(path: &Path) -> Result<bool, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{GetVolumeInformationW, GetVolumePathNameW};

    // FILE_SUPPORTS_HARD_LINKS. Named here so a missing windows-crate
    // constant cannot change the meaning; the value is from GetVolumeInformationW.
    const FILE_SUPPORTS_HARD_LINKS: u32 = 0x0040_0000;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut root = [0_u16; 520];
    unsafe { GetVolumePathNameW(PCWSTR(wide.as_ptr()), &mut root) }
        .map_err(|error| error.to_string())?;

    let mut flags = 0_u32;
    unsafe {
        GetVolumeInformationW(
            PCWSTR(root.as_ptr()),
            None,
            None,
            None,
            Some(&mut flags),
            None,
        )
    }
    .map_err(|error| error.to_string())?;

    Ok(flags & FILE_SUPPORTS_HARD_LINKS != 0)
}

/// Production first-run is Windows (DPAPI). On this crate's Linux compile
/// we cannot ask a volume the same way, so the answer is Unknown rather
/// than a guessed Supported.
#[cfg(not(target_os = "windows"))]
fn volume_reports_hard_links(_path: &Path) -> Result<bool, String> {
    Err("volume hard-link support is only queried on Windows".to_string())
}

/// Install the temp key as `storage.key` without replacing a file that
/// already won the race. `std::fs::rename` on Windows uses
/// MOVEFILE_REPLACE_EXISTING, which would let a second first-run overwrite
/// the key the first process is already using.
fn rename_without_replace(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::core::PCWSTR;
        use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_WRITE_THROUGH};

        let source_wide: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
        let destination_wide: Vec<u16> = destination
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect();
        unsafe {
            MoveFileExW(
                PCWSTR(source_wide.as_ptr()),
                PCWSTR(destination_wide.as_ptr()),
                MOVEFILE_WRITE_THROUGH,
            )
        }
        .map_err(|error| io::Error::other(error.to_string()))
    }
    #[cfg(not(target_os = "windows"))]
    {
        // Unix rename replaces. The Windows arm uses MoveFileExW without
        // MOVEFILE_REPLACE_EXISTING so a second first-run cannot overwrite
        // the winner. Mirror that here: refuse when dest is already present.
        match destination.try_exists() {
            Ok(true) => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "protected storage key already exists",
            )),
            Ok(false) => std::fs::rename(source, destination),
            Err(error) => Err(error),
        }
    }
}

#[cfg(target_os = "windows")]
fn protect_for_current_user(plaintext: &[u8]) -> Result<Vec<u8>, String> {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{
        CryptProtectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: plaintext.len() as u32,
        pbData: plaintext.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptProtectData(
            &input,
            windows::core::w!("Cubby clipboard storage key"),
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .map_err(|e| format!("Windows could not protect the storage key: {e}"))?;
        let protected = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        Ok(protected)
    }
}

#[cfg(target_os = "windows")]
fn unprotect_for_current_user(protected: &[u8]) -> Result<Vec<u8>, String> {
    use windows::Win32::Foundation::{LocalFree, HLOCAL};
    use windows::Win32::Security::Cryptography::{
        CryptUnprotectData, CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    let input = CRYPT_INTEGER_BLOB {
        cbData: protected.len() as u32,
        pbData: protected.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
        .map_err(|e| format!("Windows could not unlock the storage key: {e}"))?;
        let plaintext = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(output.pbData.cast())));
        Ok(plaintext)
    }
}

#[cfg(not(target_os = "windows"))]
fn protect_for_current_user(_plaintext: &[u8]) -> Result<Vec<u8>, String> {
    Err("Cubby encrypted storage currently requires Windows".to_string())
}

#[cfg(not(target_os = "windows"))]
fn unprotect_for_current_user(_protected: &[u8]) -> Result<Vec<u8>, String> {
    Err("Cubby encrypted storage currently requires Windows".to_string())
}

#[cfg(test)]
mod tests {
    use super::{CryptoManager, StorageError, KEY_LEN};
    use std::io;

    #[test]
    fn encrypted_payloads_round_trip_and_detect_tampering() {
        let crypto = CryptoManager::ephemeral();
        let encrypted = crypto.encrypt(b"private clipboard text").unwrap();
        assert!(crypto.is_encrypted(&encrypted));
        assert_eq!(
            crypto.decrypt(&encrypted).unwrap(),
            b"private clipboard text"
        );

        let mut tampered = encrypted;
        *tampered.last_mut().unwrap() ^= 1;
        assert!(crypto.decrypt(&tampered).is_err());
    }

    #[test]
    fn plaintext_encryption_marker_collisions_remain_plaintext() {
        let crypto = CryptoManager::ephemeral();
        assert!(!crypto.is_encrypted(b"CUB1 ordinary clipboard text that is long enough"));
        assert!(!crypto.is_encrypted_text("CUB1:not-an-encrypted-envelope"));
    }

    #[test]
    fn keyed_hash_is_stable_without_exposing_plain_sha256() {
        let crypto = CryptoManager::ephemeral();
        assert_eq!(crypto.keyed_hash(b"same"), crypto.keyed_hash(b"same"));
        assert_ne!(crypto.keyed_hash(b"same"), crypto.keyed_hash(b"different"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn persisted_key_is_dpapi_protected_and_reopens_for_the_same_user() {
        let directory =
            std::env::temp_dir().join(format!("cubby-key-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let database_path = directory.join("cubby.db");

        let first = CryptoManager::load_or_create(&database_path, true).unwrap();
        let protected_key = std::fs::read(directory.join("storage.key")).unwrap();
        assert_ne!(protected_key.len(), KEY_LEN);

        let second = CryptoManager::load_or_create(&database_path, false).unwrap();
        assert_eq!(
            first.keyed_hash(b"clipboard payload"),
            second.keyed_hash(b"clipboard payload")
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn concurrent_key_creation_converges_on_one_installed_key() {
        let directory =
            std::env::temp_dir().join(format!("cubby-key-race-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let database_path = directory.join("cubby.db");
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));

        let workers: Vec<_> = (0..8)
            .map(|_| {
                let database_path = database_path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    CryptoManager::load_or_create(&database_path, true)
                        .unwrap()
                        .keyed_hash(b"same clipboard payload")
                })
            })
            .collect();
        let hashes: Vec<_> = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect();

        assert!(hashes.iter().all(|hash| hash == &hashes[0]));
        assert!(directory.join("storage.key").exists());
        assert!(std::fs::read_dir(&directory).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp")));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn missing_key_fails_closed_once_encrypted_storage_exists() {
        let directory =
            std::env::temp_dir().join(format!("cubby-key-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let database_path = directory.join("cubby.db");
        let error = CryptoManager::load_or_create(&database_path, false)
            .err()
            .expect("missing protected key should fail");
        assert!(error.to_string().contains("storage key is missing"));
        assert!(matches!(error, StorageError::Other(_)));
        assert!(!directory.join("storage.key").exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// A key this account cannot unprotect must be reported as its own thing,
    /// not folded in with corruption or a missing file. Startup keys the
    /// recovery dialog off that distinction, and the wrong classification would
    /// send a portable user carrying their folder between accounts back to a
    /// bare panic.
    ///
    /// Bytes DPAPI will refuse stand in for another account's key: an actual
    /// cross-account key cannot be produced from inside one test process, and
    /// `CryptUnprotectData` rejects both for the same reason.
    #[cfg(target_os = "windows")]
    #[test]
    fn unreadable_key_is_reported_as_belonging_to_another_account() {
        let directory =
            std::env::temp_dir().join(format!("cubby-key-foreign-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let database_path = directory.join("cubby.db");
        let key_path = directory.join("storage.key");
        std::fs::write(&key_path, b"not a DPAPI blob this account can open").unwrap();

        let error = CryptoManager::load_or_create(&database_path, true)
            .err()
            .expect("an unreadable key must not silently create a new one");

        match &error {
            StorageError::KeyNotForThisUser {
                key_path: reported, ..
            } => assert_eq!(reported, &key_path),
            other => panic!("expected KeyNotForThisUser, got {other:?}"),
        }
        // The whole point: the user's key is still on disk, so the original
        // account can still read the database it belongs to.
        assert!(key_path.exists());
        assert_eq!(
            std::fs::read(&key_path).unwrap(),
            b"not a DPAPI blob this account can open"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn key_install_dir() -> std::path::PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "cubby-key-install-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn unsupported_hard_link_error() -> io::Error {
        #[cfg(target_os = "windows")]
        {
            io::Error::from_raw_os_error(1)
        }
        #[cfg(not(target_os = "windows"))]
        {
            io::Error::new(io::ErrorKind::Unsupported, "hard links are not supported")
        }
    }

    /// First-run on FAT/exFAT cannot CreateHardLinkW. The install must still
    /// put `storage.key` in place; otherwise Database::new panics and a
    /// portable USB never starts (SBS-908).
    #[test]
    fn storage_key_installs_when_volume_cannot_hard_link() {
        let directory = key_install_dir();
        let temporary_path = directory.join("storage.key.1.tmp");
        let key_path = directory.join("storage.key");
        std::fs::write(&temporary_path, b"protected-key-bytes").unwrap();

        let result = super::install_storage_key_file_with(
            &temporary_path,
            &key_path,
            super::VolumeHardLinkSupport::Unsupported,
            |_, _| panic!("FAT/exFAT must not call hard_link"),
        );

        assert!(matches!(result, Ok(super::KeyInstall::Installed)));
        assert_eq!(std::fs::read(&key_path).unwrap(), b"protected-key-bytes");
        assert!(!temporary_path.exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// A volume we could not classify, plus a hard-link error that is not
    /// "this filesystem cannot do this", is not FAT. Guessing a rename would
    /// treat a lock or access-denied as a successful install.
    #[test]
    fn unknown_hard_link_failure_does_not_install_by_guessing() {
        let directory = key_install_dir();
        let temporary_path = directory.join("storage.key.1.tmp");
        let key_path = directory.join("storage.key");
        std::fs::write(&temporary_path, b"protected-key-bytes").unwrap();

        let error = super::install_storage_key_file_with(
            &temporary_path,
            &key_path,
            super::VolumeHardLinkSupport::Unknown,
            |_, _| Err(io::Error::from_raw_os_error(33)),
        )
        .err()
        .expect("an unclassified hard-link failure must not install the key");

        assert!(error
            .to_string()
            .contains("volume hard-link support could not be determined"));
        assert!(!key_path.exists());
        assert!(!temporary_path.exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// The error code is a stronger signal than a failed volume probe: USB
    /// that GetVolumeInformationW could not name still has to start.
    #[test]
    fn unsupported_hard_link_error_falls_back_when_volume_is_unknown() {
        let directory = key_install_dir();
        let temporary_path = directory.join("storage.key.1.tmp");
        let key_path = directory.join("storage.key");
        std::fs::write(&temporary_path, b"protected-key-bytes").unwrap();

        let result = super::install_storage_key_file_with(
            &temporary_path,
            &key_path,
            super::VolumeHardLinkSupport::Unknown,
            |_, _| Err(unsupported_hard_link_error()),
        );

        assert!(matches!(result, Ok(super::KeyInstall::Installed)));
        assert_eq!(std::fs::read(&key_path).unwrap(), b"protected-key-bytes");
        assert!(!temporary_path.exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// A second first-run must load the key that already landed. Exclusive
    /// rename is the FAT stand-in for "hard_link fails if dest exists"; a
    /// replacing rename would let the loser overwrite the winner.
    #[test]
    fn existing_dest_wins_on_unsupported_volume() {
        let directory = key_install_dir();
        let temporary_path = directory.join("storage.key.1.tmp");
        let key_path = directory.join("storage.key");
        std::fs::write(&key_path, b"winner").unwrap();
        std::fs::write(&temporary_path, b"loser").unwrap();

        let result = super::install_storage_key_file_with(
            &temporary_path,
            &key_path,
            super::VolumeHardLinkSupport::Unsupported,
            |_, _| panic!("FAT/exFAT must not call hard_link"),
        );

        assert!(matches!(result, Ok(super::KeyInstall::AlreadyPresent)));
        assert_eq!(std::fs::read(&key_path).unwrap(), b"winner");
        assert!(!temporary_path.exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// SBS-908: the presence check is a `?` in the middle of the install, so a
    /// `try_exists` that fails (locked parent, access denied, sharing
    /// violation) used to return before the temp cleanup. Repeated failing
    /// first runs then left generated key bytes on disk under
    /// `storage.key.<pid>.<uuid>.tmp`.
    #[test]
    fn failed_presence_check_still_removes_the_temporary_key() {
        let directory = key_install_dir();
        let temporary_path = directory.join("storage.key.1.tmp");
        let key_path = directory.join("storage.key");
        std::fs::write(&temporary_path, b"protected-key-bytes").unwrap();

        let error = super::install_storage_key_file_using(
            &temporary_path,
            &key_path,
            super::VolumeHardLinkSupport::Supported,
            |_, _| Err(io::Error::from_raw_os_error(33)),
            |_| {
                Err(super::StorageError::Other(
                    "failed to inspect protected storage key path".to_string(),
                ))
            },
        )
        .err()
        .expect("an unreadable key path must not report a successful install");

        assert!(error.to_string().contains("failed to inspect"));
        assert!(!key_path.exists());
        assert!(
            !temporary_path.exists(),
            "the generated key bytes must not survive a failed presence check"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    /// A Supported volume that fails hard_link for a reason that is not
    /// "filesystem cannot do this" must not fall through to rename.
    #[test]
    fn supported_volume_does_not_rename_on_unrelated_hard_link_error() {
        let directory = key_install_dir();
        let temporary_path = directory.join("storage.key.1.tmp");
        let key_path = directory.join("storage.key");
        std::fs::write(&temporary_path, b"protected-key-bytes").unwrap();

        let error = super::install_storage_key_file_with(
            &temporary_path,
            &key_path,
            super::VolumeHardLinkSupport::Supported,
            |_, _| Err(io::Error::from_raw_os_error(33)),
        )
        .err()
        .expect("an NTFS hard-link failure is not a FAT fallback");

        assert!(!error
            .to_string()
            .contains("volume hard-link support could not be determined"));
        assert!(error
            .to_string()
            .contains("failed to install protected storage key"));
        assert!(!key_path.exists());
        assert!(!temporary_path.exists());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
