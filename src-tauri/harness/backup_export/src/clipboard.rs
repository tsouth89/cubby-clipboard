use crate::crypto::CryptoManager;
use std::path::Path;

pub fn persist_full_image_file(
    crypto: &CryptoManager,
    image_dir: &Path,
    clip_uuid: &str,
    png_bytes: &[u8],
) -> Result<String, String> {
    crate::image_persist::persist_full_image_file(crypto, image_dir, clip_uuid, png_bytes)
}

pub fn read_full_image_file(crypto: &CryptoManager, file_path: &str) -> Result<Vec<u8>, String> {
    let encrypted = std::fs::read(file_path).map_err(|e| e.to_string())?;
    crypto.decrypt(&encrypted)
}

pub fn remove_full_image_file(file_path: &str) {
    if let Err(error) = std::fs::remove_file(file_path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            log::warn!("Failed to delete a stored clipboard image: {error}");
        }
    }
}

pub(crate) fn build_clip_hash_material<'a>(
    clip_type: &str,
    primary_content: &[u8],
    formats: impl IntoIterator<Item = (&'a str, &'a [u8])>,
) -> Vec<u8> {
    let mut material = Vec::new();
    material.extend_from_slice(clip_type.as_bytes());
    material.push(0);
    material.extend_from_slice(primary_content);
    if clip_type != "image" {
        for (name, content) in formats {
            material.push(0);
            material.extend_from_slice(name.as_bytes());
            material.push(0);
            material.extend_from_slice(content);
        }
    }
    material
}
