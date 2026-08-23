//! Exclusive write for `{uuid}.cubby.tmp`.
//!
//! `std::fs::write` truncates on open. After a dest-gone leave-temp (SBS-1030)
//! that file can be the only remaining original, and same-uuid restaging —
//! expired revival retries Capture — must not destroy it (SBS-1073). Backup
//! already uses `create_new` for this class of hazard.
//!
//! No crate dependencies so `rustc --test src-tauri/src/image_stage.rs` can
//! prove the overwrite refusal on Linux. Windows CI runs the same tests via
//! `cargo test --all-targets`.

use std::io::Write;
use std::path::Path;

/// Write `bytes` only if `path` does not already exist. An existing dest-gone
/// leave-temp is left byte-identical; the caller sees a conflict error.
pub(crate) fn write_staged_image_temp(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                format!(
                    "refusing to overwrite existing staging file {}",
                    path.display()
                )
            } else {
                error.to_string()
            }
        })?;
    if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(error.to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::write_staged_image_temp;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_SEQ: AtomicU64 = AtomicU64::new(0);

    fn test_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cubby-sbs-1073-{}-{}",
            std::process::id(),
            TEST_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// SBS-1073: dest-gone leave-temp is the only remaining original. Restaging
    /// the same uuid must fail instead of truncating those bytes.
    #[test]
    fn restaging_does_not_overwrite_a_dest_gone_leave_temp() {
        let dir = test_dir();
        let dest = dir.join("abc.cubby");
        let tmp = dir.join("abc.cubby.tmp");
        fs::write(&tmp, b"only-remaining-original").unwrap();
        assert!(!dest.exists());

        let staged = write_staged_image_temp(&tmp, b"retry-full-res");
        assert!(
            staged.is_err(),
            "restaging must refuse to open the existing leave-temp: {:?}",
            staged
        );
        let error = staged.unwrap_err();
        assert!(
            error.contains("refusing to overwrite"),
            "the error should describe the staging-file conflict: {}",
            error
        );
        assert_eq!(
            fs::read(&tmp).expect("leave-temp must still exist"),
            b"only-remaining-original",
            "restaging must not truncate the dest-gone leave-temp"
        );
        assert!(
            !dest.exists(),
            "a refused restage must not invent a live original"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// A leftover temp beside a live dest is not the only copy, but restaging
    /// still must not truncate it. `create_new` treats that the same as dest-gone.
    #[test]
    fn restaging_does_not_overwrite_an_existing_temp_beside_a_live_original() {
        let dir = test_dir();
        let dest = dir.join("abc.cubby");
        let tmp = dir.join("abc.cubby.tmp");
        fs::write(&dest, b"previous-full-res").unwrap();
        fs::write(&tmp, b"leftover-staging-bytes").unwrap();

        let staged = write_staged_image_temp(&tmp, b"retry-full-res");
        assert!(
            staged.is_err(),
            "restaging must refuse to open an existing staging file: {:?}",
            staged
        );
        assert_eq!(fs::read(&dest).unwrap(), b"previous-full-res");
        assert_eq!(
            fs::read(&tmp).expect("leftover temp must still exist"),
            b"leftover-staging-bytes",
            "restaging must not truncate a leftover staging file"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn staging_writes_when_the_temp_name_is_free() {
        let dir = test_dir();
        let tmp = dir.join("abc.cubby.tmp");

        write_staged_image_temp(&tmp, b"new-full-res").expect("a free staging name must succeed");
        assert_eq!(fs::read(&tmp).unwrap(), b"new-full-res");
        let _ = fs::remove_dir_all(&dir);
    }

    /// Occupying the staging name with a directory must fail without inventing
    /// a live original. Same collision class as the persist-path test.
    #[test]
    fn staging_fails_when_the_temp_name_is_a_directory() {
        let dir = test_dir();
        let dest = dir.join("abc.cubby");
        let tmp = dir.join("abc.cubby.tmp");
        fs::write(&dest, b"previous-full-res").unwrap();
        fs::create_dir(&tmp).unwrap();

        let staged = write_staged_image_temp(&tmp, b"new-full-res");
        assert!(
            staged.is_err(),
            "a directory on the staging name must fail the write: {:?}",
            staged
        );
        assert_eq!(fs::read(&dest).unwrap(), b"previous-full-res");
        assert!(tmp.is_dir(), "the colliding directory must be left alone");
        let _ = fs::remove_dir_all(&dir);
    }
}
