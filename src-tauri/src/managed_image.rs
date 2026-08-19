//! Guarded delete for files named by `clip_images.file_path`.
//!
//! `remove_full_image_file` is a bare unlink. Every caller that is deleting a
//! path from the database must go through `remove_clip_image_files`, which
//! refuses anything whose parent is not the managed image directory (SBS-987).
//!
//! Kept free of the Windows-only crate graph so
//! `rustc --test src-tauri/src/managed_image.rs` can prove the guard on Linux.
//! Windows CI runs the same tests via `cargo test --all-targets`.

use std::path::Path;

pub(crate) fn is_managed_image_path(image_dir: &Path, file_path: &str) -> bool {
    let Ok(managed_dir) = image_dir.canonicalize() else {
        return false;
    };
    let Some(parent) = Path::new(file_path).parent() else {
        return false;
    };
    parent
        .canonicalize()
        .map(|candidate| candidate == managed_dir)
        .unwrap_or(false)
}

pub(crate) fn remove_full_image_file(file_path: &str) {
    if let Err(error) = std::fs::remove_file(file_path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            warn_delete_failed(&error);
        }
    }
}

/// Delete stored clipboard images, skipping any path that is not inside
/// `image_dir`. Empty strings are ignored. An unmanaged path is left on disk
/// and (in production) logged.
pub(crate) fn remove_clip_image_files(image_dir: &Path, image_paths: Vec<String>) {
    for path in image_paths {
        if !path.is_empty() && is_managed_image_path(image_dir, &path) {
            remove_full_image_file(&path);
        } else if !path.is_empty() {
            warn_skipped_unmanaged();
        }
    }
}

#[cfg(not(test))]
fn warn_skipped_unmanaged() {
    log::warn!("Skipped deleting an unmanaged clipboard image path");
}

#[cfg(test)]
fn warn_skipped_unmanaged() {}

#[cfg(not(test))]
fn warn_delete_failed(error: &std::io::Error) {
    log::warn!("Failed to delete a stored clipboard image: {}", error);
}

#[cfg(test)]
fn warn_delete_failed(_error: &std::io::Error) {}

#[cfg(test)]
mod tests {
    use super::{is_managed_image_path, remove_clip_image_files};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_root() -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "cubby-managed-image-{}-{}",
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// SBS-987: a path from `clip_images.file_path` that sits outside the
    /// managed image directory must survive, while a managed sibling is
    /// unlinked. This is the same helper content-hash dedup now calls.
    #[test]
    fn remove_clip_image_files_deletes_managed_and_skips_unmanaged() {
        let root = unique_root();
        let image_dir = root.join("images");
        let external_dir = root.join("external");
        std::fs::create_dir_all(&image_dir).unwrap();
        std::fs::create_dir_all(&external_dir).unwrap();
        let managed = image_dir.join("managed.cubby");
        let external = external_dir.join("taxes.pdf");
        std::fs::write(&managed, b"managed").unwrap();
        std::fs::write(&external, b"do-not-delete").unwrap();

        remove_clip_image_files(
            &image_dir,
            vec![
                managed.to_string_lossy().to_string(),
                external.to_string_lossy().to_string(),
                String::new(),
            ],
        );

        assert!(
            !managed.exists(),
            "a file in the managed image directory must be deleted"
        );
        assert!(
            external.exists(),
            "a file outside the managed image directory must not be deleted"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_dotdot_escape_is_not_a_managed_path() {
        let root = unique_root();
        let image_dir = root.join("images");
        let outside = root.join("outside");
        std::fs::create_dir_all(&image_dir).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let escaped = image_dir
            .join("..")
            .join("outside")
            .join("taxes.pdf")
            .to_string_lossy()
            .to_string();
        std::fs::write(&escaped, b"escape").unwrap();

        assert!(
            !is_managed_image_path(&image_dir, &escaped),
            "parent/.. must not count as the managed directory"
        );
        remove_clip_image_files(&image_dir, vec![escaped.clone()]);
        assert!(
            std::path::Path::new(&escaped).exists(),
            "a .. escape must not be unlinked"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Pins the call site, not just the helper. `remove_clip_image_files`
    /// already had the guard; the bug was that content-hash dedup never called
    /// it. This fails if `enforce_content_hash_uniqueness` goes back to a bare
    /// `remove_full_image_file` on `clip_images.file_path`.
    #[test]
    fn content_hash_dedup_deletes_through_the_managed_path_guard() {
        let src = include_str!("database.rs");
        let start = src
            .find("pub async fn enforce_content_hash_uniqueness")
            .expect("enforce_content_hash_uniqueness should exist");
        let rest = &src[start..];
        let end = rest[1..]
            .find("\n    pub ")
            .map(|index| index + 1)
            .unwrap_or(rest.len());
        let body = &rest[..end];
        assert!(
            body.contains("remove_clip_image_files"),
            "dedup must delete images through the managed-path guard"
        );
        assert!(
            !body.contains("remove_full_image_file"),
            "dedup must not unlink clip_images.file_path without the guard"
        );
    }
}
