//! Settings disk recovery for an interrupted `settings.json` replace (SBS-935).
//!
//! On FAT/exFAT, `MoveFileExW(REPLACE_EXISTING)` is delete-then-rename. A crash
//! after the destination is gone leaves `settings.json.tmp` holding the only
//! copy of the new preferences. That is not a first run: treating it as
//! `AppSettings::default()` would adopt 30-day retention and then overwrite
//! the leftover temp.
//!
//! `recover_dest_gone_replace` is the shared dest-gone fallback: settings save,
//! backup persist, image `{uuid}.cubby` replace (SBS-1030), and rolling
//! `cubby.db.bak` install (SBS-1051) all call it after a failed replace so
//! Drop/cleanup cannot delete the only remaining copy.
//!
//! This file has no crate dependencies so `rustc --test` can pin the contract
//! on a Linux box that cannot compile the Windows crate.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Same sibling name `SettingsManager::save` writes before replace.
pub fn settings_tmp_path(canonical: &Path) -> PathBuf {
    canonical.with_extension("json.tmp")
}

/// Write the next settings file without overwriting a lone recovery temp.
/// When the canonical file exists it remains the fallback, so a stale temp is
/// safe to replace. Without that file, an existing temp is the only disk copy
/// of the user's preferences and must stay untouched.
pub fn write_settings_temp(canonical: &Path, tmp: &Path, bytes: &[u8]) -> Result<(), String> {
    if canonical.is_file() {
        return fs::write(tmp, bytes).map_err(|error| error.to_string());
    }

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp)
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                format!(
                    "refusing to overwrite settings recovery temp {}",
                    tmp.display()
                )
            } else {
                error.to_string()
            }
        })?;
    if let Err(error) = file.write_all(bytes) {
        drop(file);
        let _ = fs::remove_file(tmp);
        return Err(error.to_string());
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsDiskSource {
    Canonical,
    InterruptedTmp,
    Missing,
}

pub fn resolve_settings_disk_source(canonical: &Path) -> SettingsDiskSource {
    if canonical.exists() {
        SettingsDiskSource::Canonical
    } else if settings_tmp_path(canonical).exists() {
        SettingsDiskSource::InterruptedTmp
    } else {
        SettingsDiskSource::Missing
    }
}

/// First-run `AppSettings::default()` (30-day) must not be written over a
/// leftover `settings.json.tmp`.
pub fn may_persist_first_run_defaults(source: SettingsDiskSource) -> bool {
    !matches!(source, SettingsDiskSource::InterruptedTmp)
}

/// Promote the leftover temp into `settings.json` so the interrupted replace
/// completes without rewriting the recovered preferences as defaults.
pub fn promote_interrupted_tmp(canonical: &Path) -> Result<(), String> {
    let tmp = settings_tmp_path(canonical);
    if canonical.exists() {
        return Ok(());
    }
    if !tmp.exists() {
        return Err("interrupted settings temp is gone".to_string());
    }
    fs::rename(&tmp, canonical).map_err(|e| e.to_string())
}

/// On exFAT/FAT, replace is delete-then-rename. If dest is gone, put the temp
/// in place instead of deleting the only copy (same idea as `backup.rs`).
pub fn recover_dest_gone_replace(tmp: &Path, dest: &Path) -> bool {
    !dest.exists() && tmp.exists() && fs::rename(tmp, dest).is_ok()
}

/// After a failed replace: recover dest-gone, else delete the temp only when
/// dest still exists. Returns true when the temp was promoted onto dest
/// (caller should treat the replace as succeeded).
///
/// Rolling `cubby.db.bak` install used `inspect_err` to always delete the
/// temp (SBS-1051). That wipes the only remaining recovery copy after a FAT
/// dest-gone mid-replace.
pub fn recover_or_discard_replace_temp(tmp: &Path, dest: &Path) -> bool {
    if recover_dest_gone_replace(tmp, dest) {
        return true;
    }
    if dest.is_file() {
        let _ = fs::remove_file(tmp);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_SEQ: AtomicU64 = AtomicU64::new(0);

    fn test_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cubby-sbs-935-{}-{}",
            std::process::id(),
            TEST_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn parse_auto_delete_days(json: &str) -> Option<i64> {
        let after = json.split_once("\"auto_delete_days\"")?.1.trim_start();
        let after = after.strip_prefix(':')?.trim_start();
        let end = after
            .find(|c: char| c != '-' && !c.is_ascii_digit())
            .unwrap_or(after.len());
        after[..end].parse().ok()
    }

    /// SBS-935: leftover `settings.json.tmp` with keep-forever must load as
    /// retention 0, and a first-run defaults save must not destroy that temp.
    #[test]
    fn interrupted_replace_recovers_tmp_retention_and_does_not_save_defaults() {
        let dir = test_dir();
        let canonical = dir.join("settings.json");
        let tmp = settings_tmp_path(&canonical);
        fs::write(&tmp, "{\n  \"auto_delete_days\": 0\n}\n").unwrap();
        assert!(!canonical.exists());

        let source = resolve_settings_disk_source(&canonical);
        assert_eq!(source, SettingsDiskSource::InterruptedTmp);
        assert!(
            !may_persist_first_run_defaults(source),
            "recovered-from-missing must not persist AppSettings::default() over the leftover temp"
        );

        let days = parse_auto_delete_days(&fs::read_to_string(&tmp).unwrap()).unwrap();
        assert_eq!(days, 0, "the leftover temp is the keep-forever choice");

        // Production persist for an un-seeded interrupted write: promote the
        // leftover temp. The old path wrote AppSettings::default() (30) onto
        // the same tmp name, then replaced, wiping keep-forever.
        if may_persist_first_run_defaults(source) {
            fs::write(&tmp, "{\n  \"auto_delete_days\": 30\n}\n").unwrap();
        } else {
            promote_interrupted_tmp(&canonical).unwrap();
        }

        assert!(canonical.exists(), "interrupted write should be completed");
        assert!(
            !tmp.exists(),
            "promoted temp should no longer sit beside dest"
        );
        let on_disk = parse_auto_delete_days(&fs::read_to_string(&canonical).unwrap()).unwrap();
        assert_eq!(on_disk, 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn true_first_run_may_persist_product_defaults() {
        let dir = test_dir();
        let canonical = dir.join("settings.json");
        let source = resolve_settings_disk_source(&canonical);
        assert_eq!(source, SettingsDiskSource::Missing);
        assert!(may_persist_first_run_defaults(source));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn later_save_does_not_overwrite_a_lone_recovery_temp() {
        let dir = test_dir();
        let canonical = dir.join("settings.json");
        let tmp = settings_tmp_path(&canonical);
        let recovered = b"{\"auto_delete_days\":0}";
        fs::write(&tmp, recovered).unwrap();

        let error = write_settings_temp(&canonical, &tmp, b"{\"auto_delete_days\":30}")
            .expect_err("a lone recovery temp must not be overwritten");

        assert!(error.contains("refusing to overwrite"));
        assert_eq!(fs::read(&tmp).unwrap(), recovered);
        assert!(!canonical.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_temp_can_be_rewritten_when_canonical_settings_exist() {
        let dir = test_dir();
        let canonical = dir.join("settings.json");
        let tmp = settings_tmp_path(&canonical);
        fs::write(&canonical, b"last-good-settings").unwrap();
        fs::write(&tmp, b"stale-temp").unwrap();

        write_settings_temp(&canonical, &tmp, b"next-settings").unwrap();

        assert_eq!(fs::read(&canonical).unwrap(), b"last-good-settings");
        assert_eq!(fs::read(&tmp).unwrap(), b"next-settings");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dest_gone_rename_puts_temp_in_place() {
        let dir = test_dir();
        let dest = dir.join("settings.json");
        let tmp = settings_tmp_path(&dest);
        fs::write(&tmp, "{\n  \"auto_delete_days\": 0\n}\n").unwrap();
        assert!(recover_dest_gone_replace(&tmp, &dest));
        assert!(dest.exists());
        assert!(!tmp.exists());
        let on_disk = parse_auto_delete_days(&fs::read_to_string(&dest).unwrap()).unwrap();
        assert_eq!(on_disk, 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn dest_gone_rename_does_not_touch_an_existing_destination() {
        let dir = test_dir();
        let dest = dir.join("settings.json");
        let tmp = settings_tmp_path(&dest);
        fs::write(&dest, "{\n  \"auto_delete_days\": 365\n}\n").unwrap();
        fs::write(&tmp, "{\n  \"auto_delete_days\": 0\n}\n").unwrap();
        assert!(!recover_dest_gone_replace(&tmp, &dest));
        assert!(tmp.exists());
        let on_disk = parse_auto_delete_days(&fs::read_to_string(&dest).unwrap()).unwrap();
        assert_eq!(on_disk, 365);
        let _ = fs::remove_dir_all(&dir);
    }

    /// SBS-1051: rolling backup install used to `inspect_err`-delete the temp
    /// after a dest-gone replace. Recover first; cleanup then finds no temp.
    #[test]
    fn rolling_backup_dest_gone_recovers_instead_of_deleting_the_only_copy() {
        let dir = test_dir();
        let dest = dir.join("cubby.db.bak");
        let tmp = dir.join("cubby.db.bak.1.uuid.tmp");
        fs::write(&tmp, b"new-rolling-backup").unwrap();
        assert!(!dest.exists());

        let recovered = recover_or_discard_replace_temp(&tmp, &dest);
        if !recovered && tmp.exists() {
            // Old inspect_err after a failed replace_backup_atomically.
            let _ = fs::remove_file(&tmp);
        }

        assert!(recovered, "dest-gone must put the staged backup in place");
        assert!(dest.exists(), "the rolling bak path must exist again");
        assert!(!tmp.exists(), "recovery consumes the temp");
        assert_eq!(fs::read(&dest).unwrap(), b"new-rolling-backup");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_replace_that_left_dest_discards_the_temp() {
        let dir = test_dir();
        let dest = dir.join("cubby.db.bak");
        let tmp = dir.join("cubby.db.bak.1.uuid.tmp");
        fs::write(&dest, b"old-rolling-backup").unwrap();
        fs::write(&tmp, b"new-rolling-backup").unwrap();

        assert!(!recover_or_discard_replace_temp(&tmp, &dest));
        assert_eq!(fs::read(&dest).unwrap(), b"old-rolling-backup");
        assert!(
            !tmp.exists(),
            "a dest that survived must still drop the temp"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// SBS-1030: image persist Drop used to delete `{uuid}.cubby.tmp` after a
    /// dest-gone replace. That is both copies gone. Recover first; Drop then
    /// finds no temp.
    #[test]
    fn image_persist_dest_gone_recovers_instead_of_deleting_the_only_copy() {
        let dir = test_dir();
        let dest = dir.join("abc.cubby");
        let tmp = dir.join("abc.cubby.tmp");
        fs::write(&tmp, b"new-full-res").unwrap();
        assert!(!dest.exists());

        let recovered = recover_dest_gone_replace(&tmp, &dest);
        if !recovered && tmp.exists() {
            // Old StagedImageFile::drop after a failed commit.
            let _ = fs::remove_file(&tmp);
        }

        assert!(recovered, "dest-gone must put the staged original in place");
        assert!(dest.exists(), "the live original path must exist again");
        assert!(!tmp.exists(), "recovery consumes the temp");
        assert_eq!(fs::read(&dest).unwrap(), b"new-full-res");
        let _ = fs::remove_dir_all(&dir);
    }
}
