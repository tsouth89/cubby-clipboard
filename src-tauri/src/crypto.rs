use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::path::Path;

const ENVELOPE_MAGIC: &[u8; 4] = b"CUB1";
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;
const TEXT_PREFIX: &str = "CUB1:";

fn load_protected_key(key_path: &Path) -> Result<[u8; KEY_LEN], String> {
    let protected = std::fs::read(key_path)
        .map_err(|e| format!("failed to read protected storage key: {e}"))?;
    let plaintext = unprotect_for_current_user(&protected)?;
    plaintext
        .try_into()
        .map_err(|_| "protected storage key has an invalid length".to_string())
}

#[derive(Clone)]
pub struct CryptoManager {
    key: [u8; KEY_LEN],
}

impl CryptoManager {
    pub fn load_or_create(db_path: &Path, allow_create: bool) -> Result<Self, String> {
        let key_path = db_path.with_file_name("storage.key");
        let key = if key_path.exists() {
            load_protected_key(&key_path)?
        } else {
            if !allow_create {
                return Err(
                    "encrypted clipboard history exists, but its protected storage key is missing"
                        .to_string(),
                );
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
            match std::fs::hard_link(&temporary_path, &key_path) {
                Ok(()) => {
                    let _ = std::fs::remove_file(&temporary_path);
                    key
                }
                Err(_) if key_path.exists() => {
                    let _ = std::fs::remove_file(&temporary_path);
                    load_protected_key(&key_path)?
                }
                Err(error) => {
                    let _ = std::fs::remove_file(&temporary_path);
                    return Err(format!("failed to install protected storage key: {error}"));
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
    use super::{CryptoManager, KEY_LEN};

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
        assert!(error.contains("storage key is missing"));
        assert!(!directory.join("storage.key").exists());
        std::fs::remove_dir_all(directory).unwrap();
    }
}
