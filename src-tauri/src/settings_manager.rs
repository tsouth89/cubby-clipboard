use crate::database::Database;
use crate::models::AppSettings;
use crate::settings_load::{
    may_persist_first_run_defaults, promote_interrupted_tmp, recover_dest_gone_replace,
    resolve_settings_disk_source, settings_tmp_path, write_settings_temp, SettingsDiskSource,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use tauri::AppHandle;
#[cfg(not(debug_assertions))]
use tauri::Manager;

pub struct SettingsManager {
    file_path: PathBuf,
    settings: RwLock<AppSettings>,
}

enum DiskRead {
    Settings {
        // Boxed: AppSettings is ~226 bytes and `Missing` carries nothing, so
        // an inline field makes every DiskRead that size.
        settings: Box<AppSettings>,
        recovered_unreadable: bool,
        recovered_interrupted: bool,
    },
    Missing,
}

/// How the loaded settings reached memory, as far as the disk is concerned.
/// Drives the one write `SettingsManager::new` is allowed to make.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SettingsOrigin {
    /// The file that should have held preferences could not be parsed and has
    /// been quarantined, so nothing on that path can still be lost.
    recovered_unreadable: bool,
    /// The preferences came from a leftover `settings.json.tmp`.
    recovered_interrupted: bool,
}

impl SettingsManager {
    pub async fn new(app: &AppHandle, db: &Database) -> Self {
        // Keep settings on the same data root as the database (including the
        // debug `/dev` isolation from SOU-227 and portable mode).
        let base = crate::get_data_dir();
        let path = base.join("settings.json");

        // Check the canonical path (and its leftover temp) before
        // resolve_settings_load_path. In release that helper copies a legacy
        // AppData file onto `path` when `path` is missing, which would hide an
        // interrupted FAT/exFAT replace sitting in settings.json.tmp (SBS-935).
        let (mut settings, origin) = match Self::read_settings_from_disk(&path) {
            DiskRead::Settings {
                settings,
                recovered_unreadable,
                recovered_interrupted,
            } => {
                let origin = SettingsOrigin {
                    recovered_unreadable,
                    recovered_interrupted,
                };
                // The unreadable file has already been quarantined, so looking
                // further cannot lose anything. A legacy AppData file that
                // still parses holds the user's real preferences; safe defaults
                // turn retention off for someone who never asked for that.
                let settings = if recovered_unreadable {
                    Self::settings_from_load_path(&Self::resolve_settings_load_path(
                        app, &base, &path,
                    ))
                    .unwrap_or(*settings)
                } else {
                    *settings
                };
                (settings, origin)
            }
            DiskRead::Missing => {
                let load_path = Self::resolve_settings_load_path(app, &base, &path);
                if load_path.exists() {
                    match Self::parse_settings_file(&load_path) {
                        Ok(settings) => (
                            settings,
                            SettingsOrigin {
                                recovered_unreadable: false,
                                recovered_interrupted: false,
                            },
                        ),
                        Err(error) => (
                            Self::recover_from_unreadable_settings(&load_path, &error),
                            SettingsOrigin {
                                recovered_unreadable: true,
                                recovered_interrupted: false,
                            },
                        ),
                    }
                } else {
                    (
                        Self::migrate_from_sqlite(db).await,
                        SettingsOrigin {
                            recovered_unreadable: false,
                            recovered_interrupted: false,
                        },
                    )
                }
            }
        };

        let seeded_defaults = Self::seed_default_sensitive_apps(&mut settings);
        let manager = Self {
            file_path: path,
            settings: RwLock::new(settings.clone()),
        };
        manager.persist_after_load(settings, origin, seeded_defaults);
        manager
    }

    /// The disk half of [`SettingsManager::new`].
    ///
    /// Split out so a test can drive it against a real [`SettingsManager::save`]
    /// without a Tauri `AppHandle`. Reverting either rule below has to fail
    /// `interrupted_tmp_is_promoted_before_any_write` or
    /// `legacy_in_place_still_writes_the_canonical_file`.
    fn persist_after_load(
        &self,
        settings: AppSettings,
        origin: SettingsOrigin,
        seeded_defaults: bool,
    ) {
        self.persist_after_load_using(settings, origin, seeded_defaults, promote_interrupted_tmp)
    }

    /// `promote` is injected so a test can fail the rename the way a sharing
    /// violation or a read-only parent does. That failure is the one path where
    /// the leftover temp is still the only copy of the preferences.
    fn persist_after_load_using(
        &self,
        settings: AppSettings,
        origin: SettingsOrigin,
        seeded_defaults: bool,
        promote: impl Fn(&Path) -> Result<(), String>,
    ) {
        // An interrupted replace leaves settings.json.tmp holding the only copy
        // of the preferences, and `save` writes that same temp name. Rename it
        // into place before anything else writes, so a disk-full or a crash
        // during a seed rewrite cannot truncate the one file that still has the
        // user's settings. If the rename fails, leave those bytes alone and
        // write nothing at all (SBS-935).
        if origin.recovered_interrupted && !origin.recovered_unreadable {
            if let Err(error) = promote(&self.file_path) {
                log::error!(
                    "SETTINGS: Failed to promote interrupted settings write: {error}; leaving {} untouched",
                    settings_tmp_path(&self.file_path).display()
                );
                return;
            }
        }

        // A missing canonical file is a true first run, a SQLite migration, or
        // a legacy AppData file that had to be loaded in place because the copy
        // failed. All three have to write settings.json, or the next launch
        // reads the legacy path again and the canonical one never appears.
        let canonical_missing = !self.file_path.exists();
        let source = resolve_settings_disk_source(&self.file_path);
        if seeded_defaults || (canonical_missing && may_persist_first_run_defaults(source)) {
            if let Err(error) = self.save(settings) {
                log::error!("SETTINGS: Failed to persist settings: {error}");
            }
        }
    }

    /// A settings file that still parses, for use only after the file that
    /// should have held the preferences was quarantined.
    ///
    /// Takes the resolved path rather than the `AppHandle` so this half is
    /// testable: `resolve_settings_load_path`'s legacy AppData branch is
    /// `cfg(not(debug_assertions))` and does not exist in a test build.
    ///
    /// Deliberately does not fall through to the SQLite migration: a key that
    /// is absent there comes back as `AppSettings::default()` 30-day retention,
    /// which is the mass-delete the safe defaults exist to avoid.
    fn settings_from_load_path(load_path: &Path) -> Option<AppSettings> {
        if !load_path.exists() {
            return None;
        }
        Self::parse_settings_file(load_path).ok()
    }

    fn parse_settings_file(path: &Path) -> Result<AppSettings, String> {
        fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|content| serde_json::from_str(&content).map_err(|e| e.to_string()))
    }

    /// Read the canonical settings file, or a leftover sibling temp from an
    /// interrupted replace. Does not consult legacy AppData or SQLite.
    fn read_settings_from_disk(canonical: &Path) -> DiskRead {
        match resolve_settings_disk_source(canonical) {
            SettingsDiskSource::Canonical => match Self::parse_settings_file(canonical) {
                Ok(settings) => DiskRead::Settings {
                    settings: Box::new(settings),
                    recovered_unreadable: false,
                    recovered_interrupted: false,
                },
                Err(error) => DiskRead::Settings {
                    settings: Box::new(Self::recover_from_unreadable_settings(canonical, &error)),
                    recovered_unreadable: true,
                    recovered_interrupted: false,
                },
            },
            SettingsDiskSource::InterruptedTmp => {
                let tmp = settings_tmp_path(canonical);
                match Self::parse_settings_file(&tmp) {
                    Ok(settings) => {
                        log::warn!(
                            "SETTINGS: settings.json is missing; recovered interrupted write from {} (SBS-935)",
                            tmp.display()
                        );
                        DiskRead::Settings {
                            settings: Box::new(settings),
                            recovered_unreadable: false,
                            recovered_interrupted: true,
                        }
                    }
                    Err(error) => DiskRead::Settings {
                        settings: Box::new(Self::recover_from_unreadable_settings(&tmp, &error)),
                        recovered_unreadable: true,
                        recovered_interrupted: true,
                    },
                }
            }
            SettingsDiskSource::Missing => DiskRead::Missing,
        }
    }

    /// Prefer the canonical settings path. In release builds, migrate once from
    /// the legacy Tauri identifier-based AppData file. Never copy release
    /// preferences into the debug `/dev` tree.
    fn resolve_settings_load_path(app: &AppHandle, base: &Path, path: &Path) -> PathBuf {
        if path.exists() {
            return path.to_path_buf();
        }

        #[cfg(debug_assertions)]
        {
            let _ = (app, base);
            path.to_path_buf()
        }

        #[cfg(not(debug_assertions))]
        {
            let Ok(legacy_base) = app.path().app_data_dir() else {
                return path.to_path_buf();
            };
            let legacy = legacy_base.join("settings.json");
            if !legacy.exists() || legacy == path {
                return path.to_path_buf();
            }

            match fs::create_dir_all(base).and_then(|_| {
                fs::copy(&legacy, path)?;
                Ok(())
            }) {
                Ok(()) => path.to_path_buf(),
                Err(error) => {
                    log::warn!(
                        "SETTINGS: Could not migrate legacy settings to {}: {error}. Loading legacy file in place.",
                        path.display()
                    );
                    legacy
                }
            }
        }
    }

    /// A settings file that exists but cannot be parsed is evidence of a bad
    /// write (power loss, disk full), not of user intent. Preserve the file for
    /// inspection and fall back to defaults with retention disabled — silently
    /// adopting the default 30-day window would mass-delete the history of a
    /// user who had chosen "keep forever".
    fn recover_from_unreadable_settings(load_path: &Path, error: &str) -> AppSettings {
        let backup = load_path.with_extension(format!(
            "json.corrupt-{}-{}",
            chrono::Local::now().format("%Y%m%d-%H%M%S"),
            uuid::Uuid::new_v4()
        ));
        match fs::rename(load_path, &backup) {
            Ok(()) => log::error!(
                "SETTINGS: settings.json is unreadable ({error}); quarantined it at {} and loading safe defaults",
                backup.display()
            ),
            Err(quarantine_error) => log::error!(
                "SETTINGS: settings.json is unreadable ({error}) and could not be quarantined ({quarantine_error}); replacing it with safe defaults"
            ),
        }
        Self::safe_default_settings()
    }

    /// Defaults used when the user's real preferences are unknown: identical to
    /// [`AppSettings::default`] except nothing is ever auto-deleted.
    fn safe_default_settings() -> AppSettings {
        AppSettings {
            auto_delete_days: 0,
            ..AppSettings::default()
        }
    }

    /// Insert the built-in password-manager executables the first time settings
    /// load. Returns true when the settings object was mutated and should be
    /// persisted. Users can remove any entry afterward; seeding will not run
    /// again once `default_sensitive_apps_seeded` is true.
    fn seed_default_sensitive_apps(settings: &mut AppSettings) -> bool {
        if settings.default_sensitive_apps_seeded {
            return false;
        }
        for exe in crate::secrets::DEFAULT_SENSITIVE_APP_EXES {
            settings.ignored_apps.insert((*exe).to_string());
        }
        settings.default_sensitive_apps_seeded = true;
        true
    }

    async fn migrate_from_sqlite(db: &Database) -> AppSettings {
        let mut settings = AppSettings::default();
        let pool = &db.pool;

        async fn get_val(pool: &sqlx::SqlitePool, key: &str) -> Option<String> {
            sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
                .bind(key)
                .fetch_optional(pool)
                .await
                .unwrap_or(None)
        }

        if let Some(v) = get_val(pool, "theme").await {
            settings.theme = v;
        }
        if let Some(v) = get_val(pool, "mica_effect").await {
            settings.mica_effect = v;
        }
        // Retention is time-only. Ignore any persisted item cap (legacy installs
        // may carry a nonzero max_items from before this was exposed) so the age
        // window is the sole lever; max_items stays 0 = no count cap.
        if let Some(v) = get_val(pool, "auto_delete_days").await {
            if let Ok(i) = v.parse() {
                settings.auto_delete_days = i;
            }
        }
        if let Some(v) = get_val(pool, "hotkey").await {
            settings.hotkey = v;
        }

        if let Some(v) = get_val(pool, "ignore_ghost_clips").await {
            if let Ok(b) = v.parse() {
                settings.ignore_ghost_clips = b;
            }
        }

        // Ignored Apps
        if let Ok(apps) = sqlx::query_scalar::<_, String>("SELECT app_name FROM ignored_apps")
            .fetch_all(pool)
            .await
        {
            settings.ignored_apps = apps.into_iter().collect();
        }

        settings
    }

    pub fn get(&self) -> AppSettings {
        self.settings.read().unwrap().clone()
    }

    pub fn save(&self, new_settings: AppSettings) -> Result<(), String> {
        let json = serde_json::to_string_pretty(&new_settings).map_err(|e| e.to_string())?;
        if let Some(parent) = self.file_path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        // Hold the write lock across the file write so concurrent saves cannot
        // interleave, and swap in a temp file so a crash mid-write never leaves
        // a truncated settings.json (a truncated file used to silently reset
        // every preference on the next launch).
        let mut lock = self.settings.write().unwrap();
        let tmp_path = settings_tmp_path(&self.file_path);
        write_settings_temp(&self.file_path, &tmp_path, json.as_bytes())?;
        if let Err(error) = replace_file_atomically(&tmp_path, &self.file_path) {
            // On exFAT/FAT, replace is delete-the-target-then-rename. A failure
            // after the destination entry is gone leaves the temp holding the
            // only copy of the preferences, so deleting it here would destroy
            // both the old file and the new one. Try to put it in place instead
            // (same dest-gone fallback as backup.rs; SBS-935).
            if recover_dest_gone_replace(&tmp_path, &self.file_path) {
                log::warn!(
                    "SETTINGS: replacing settings.json failed ({error}); the new file was renamed into place instead"
                );
                *lock = new_settings;
                return Ok(());
            }
            if self.file_path.is_file() {
                let _ = fs::remove_file(&tmp_path);
            } else {
                log::error!(
                    "SETTINGS: replacing settings.json failed ({error}) and the destination is not a file; leaving {}",
                    tmp_path.display()
                );
            }
            return Err(error);
        }
        *lock = new_settings;

        Ok(())
    }
}

#[cfg(target_os = "windows")]
fn replace_file_atomically(source: &Path, destination: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source_wide = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination_wide = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();

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
fn replace_file_atomically(source: &Path, destination: &Path) -> Result<(), String> {
    fs::rename(source, destination).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_defaults_never_auto_delete() {
        let recovered = SettingsManager::safe_default_settings();
        assert_eq!(recovered.auto_delete_days, 0);
        assert_eq!(recovered.max_items, 0);
    }

    #[test]
    fn pre_i18n_removal_settings_ignore_the_language_key() {
        let dir =
            std::env::temp_dir().join(format!("cubby-settings-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let mut persisted = serde_json::to_value(AppSettings::default()).unwrap();
        persisted
            .as_object_mut()
            .unwrap()
            .insert("language".to_string(), serde_json::json!("de"));
        fs::write(&path, serde_json::to_string_pretty(&persisted).unwrap()).unwrap();

        let loaded = SettingsManager::parse_settings_file(&path)
            .expect("obsolete language setting must not break an upgrade");
        assert_eq!(loaded.theme, AppSettings::default().theme);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn recovery_preserves_corrupt_file_and_disables_retention() {
        let dir =
            std::env::temp_dir().join(format!("cubby-settings-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        fs::write(&path, "{\"theme\": \"dark\", TRUNCATED").unwrap();

        let recovered = SettingsManager::recover_from_unreadable_settings(&path, "parse error");
        assert_eq!(recovered.auto_delete_days, 0);
        assert!(!path.exists());

        let corrupt_copies = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains("json.corrupt-")
            })
            .count();
        assert_eq!(corrupt_copies, 1);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_replaces_file_atomically_and_updates_cache() {
        let dir =
            std::env::temp_dir().join(format!("cubby-settings-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        fs::write(
            &path,
            serde_json::to_string(&AppSettings::default()).unwrap(),
        )
        .unwrap();

        let manager = SettingsManager {
            file_path: path.clone(),
            settings: RwLock::new(AppSettings::default()),
        };
        let updated = AppSettings {
            auto_delete_days: 365,
            ..AppSettings::default()
        };
        manager.save(updated).unwrap();

        assert_eq!(manager.get().auto_delete_days, 365);
        let on_disk: AppSettings =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk.auto_delete_days, 365);
        assert!(!path.with_extension("json.tmp").exists());

        let _ = fs::remove_dir_all(&dir);
    }

    /// If settings.json is a directory, replace cannot succeed. Deleting the
    /// temp would throw away the only copy of the new preferences.
    #[test]
    fn save_leaves_tmp_when_destination_is_a_directory() {
        let dir =
            std::env::temp_dir().join(format!("cubby-settings-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        fs::create_dir_all(&path).unwrap();

        let manager = SettingsManager {
            file_path: path.clone(),
            settings: RwLock::new(AppSettings::default()),
        };
        let updated = AppSettings {
            auto_delete_days: 365,
            ..AppSettings::default()
        };
        manager
            .save(updated)
            .expect_err("replacing a directory must fail");
        assert!(
            settings_tmp_path(&path).exists(),
            "the new preferences must remain in the temp"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// SBS-935: an interrupted replace leaves settings.json.tmp and no
    /// settings.json. Load must recover keep-forever (0), not first-run 30,
    /// and must not persist AppSettings::default() over the leftover temp.
    #[test]
    fn interrupted_tmp_recovers_retention_and_is_not_overwritten_by_defaults() {
        let dir =
            std::env::temp_dir().join(format!("cubby-settings-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let tmp = settings_tmp_path(&path);
        let recovered = AppSettings {
            auto_delete_days: 0,
            default_sensitive_apps_seeded: true,
            has_completed_onboarding: true,
            ..AppSettings::default()
        };
        fs::write(&tmp, serde_json::to_string_pretty(&recovered).unwrap()).unwrap();
        assert!(!path.exists());

        let source = resolve_settings_disk_source(&path);
        assert_eq!(source, SettingsDiskSource::InterruptedTmp);
        assert!(!may_persist_first_run_defaults(source));

        match SettingsManager::read_settings_from_disk(&path) {
            DiskRead::Settings {
                settings,
                recovered_unreadable,
                recovered_interrupted,
            } => {
                assert!(!recovered_unreadable);
                assert!(recovered_interrupted);
                assert_eq!(settings.auto_delete_days, 0);
                assert!(settings.has_completed_onboarding);
                assert_ne!(
                    settings.auto_delete_days,
                    AppSettings::default().auto_delete_days
                );

                // Seeded flag is already true, so new() promotes the leftover
                // temp rather than saving AppSettings::default() over it.
                let mut loaded = settings;
                let seeded = SettingsManager::seed_default_sensitive_apps(&mut loaded);
                assert!(!seeded);
                promote_interrupted_tmp(&path).unwrap();
            }
            DiskRead::Missing => panic!("leftover settings.json.tmp must not be a first-run miss"),
        }

        assert!(path.exists());
        assert!(!tmp.exists());
        let on_disk: AppSettings =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk.auto_delete_days, 0);
        assert!(on_disk.has_completed_onboarding);

        let _ = fs::remove_dir_all(&dir);
    }

    fn manager_at(path: &std::path::Path, settings: AppSettings) -> SettingsManager {
        SettingsManager {
            file_path: path.to_path_buf(),
            settings: RwLock::new(settings),
        }
    }

    /// SBS-935: the leftover temp is the only copy of the preferences, and
    /// `save` writes that same temp name. `persist_after_load` must rename it
    /// into place first, and must write nothing at all when that rename fails —
    /// otherwise a seed rewrite truncates the one file that still has the
    /// user's settings.
    #[test]
    fn interrupted_tmp_is_promoted_before_any_write() {
        let dir =
            std::env::temp_dir().join(format!("cubby-settings-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let tmp = settings_tmp_path(&path);
        // seeded = false is the case that used to reach save(): an older temp
        // written before default_sensitive_apps_seeded existed defaults to
        // false, so seeding mutates the object and asks for a persist.
        let recovered = AppSettings {
            auto_delete_days: 0,
            default_sensitive_apps_seeded: false,
            ..AppSettings::default()
        };
        let recovered_bytes = serde_json::to_string_pretty(&recovered).unwrap();
        fs::write(&tmp, &recovered_bytes).unwrap();

        let mut loaded = recovered.clone();
        let seeded = SettingsManager::seed_default_sensitive_apps(&mut loaded);
        assert!(seeded, "an older temp must still ask for the one-time seed");

        let manager = manager_at(&path, loaded.clone());
        manager.persist_after_load(
            loaded,
            SettingsOrigin {
                recovered_unreadable: false,
                recovered_interrupted: true,
            },
            seeded,
        );

        assert!(path.exists(), "the interrupted replace must be completed");
        let on_disk: AppSettings =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            on_disk.auto_delete_days, 0,
            "keep-forever must survive the seed rewrite"
        );
        assert!(on_disk.default_sensitive_apps_seeded);
        let _ = fs::remove_dir_all(&dir);
    }

    /// The same load with a promote that cannot succeed. `settings.json.tmp`
    /// is the only copy, so a failed promote must leave those bytes exactly as
    /// they were rather than truncating them with `fs::write`.
    #[test]
    fn a_failed_promote_leaves_the_only_copy_untouched() {
        let dir =
            std::env::temp_dir().join(format!("cubby-settings-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let tmp = settings_tmp_path(&path);
        let recovered = AppSettings {
            auto_delete_days: 0,
            default_sensitive_apps_seeded: false,
            ..AppSettings::default()
        };
        let recovered_bytes = serde_json::to_string_pretty(&recovered).unwrap();
        fs::write(&tmp, &recovered_bytes).unwrap();

        let mut loaded = recovered.clone();
        let seeded = SettingsManager::seed_default_sensitive_apps(&mut loaded);
        let manager = manager_at(&path, loaded.clone());
        manager.persist_after_load_using(
            loaded,
            SettingsOrigin {
                recovered_unreadable: false,
                recovered_interrupted: true,
            },
            seeded,
            |_| Err("sharing violation".to_string()),
        );

        assert_eq!(
            fs::read_to_string(&tmp).unwrap(),
            recovered_bytes,
            "a failed promote must not rewrite the only copy of the preferences"
        );
        assert!(
            !path.exists(),
            "nothing may be written while the temp still holds the only copy"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_after_failed_promote_leaves_the_only_copy_untouched() {
        let dir =
            std::env::temp_dir().join(format!("cubby-settings-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let tmp = settings_tmp_path(&path);
        let recovered = AppSettings {
            auto_delete_days: 0,
            ..AppSettings::default()
        };
        let recovered_bytes = serde_json::to_string_pretty(&recovered).unwrap();
        fs::write(&tmp, &recovered_bytes).unwrap();

        let manager = manager_at(&path, recovered.clone());
        let changed = AppSettings {
            auto_delete_days: 30,
            ..recovered.clone()
        };
        let error = manager
            .save(changed)
            .expect_err("save must refuse to overwrite the recovery temp");

        assert!(error.contains("refusing to overwrite"));
        assert_eq!(fs::read_to_string(&tmp).unwrap(), recovered_bytes);
        assert!(!path.exists());
        assert_eq!(manager.get().auto_delete_days, recovered.auto_delete_days);
        let _ = fs::remove_dir_all(&dir);
    }

    /// SBS-935 regression guard: the persist trigger used to be
    /// `!file_path.exists()`. A legacy AppData file loaded in place (the copy
    /// onto the canonical path failed) parses fine and is already seeded, so a
    /// trigger that only looks at those two flags never writes settings.json
    /// and every launch reads the legacy path again.
    #[test]
    fn legacy_in_place_still_writes_the_canonical_file() {
        let dir =
            std::env::temp_dir().join(format!("cubby-settings-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let loaded = AppSettings {
            auto_delete_days: 365,
            default_sensitive_apps_seeded: true,
            ..AppSettings::default()
        };
        let seeded = false;

        let manager = manager_at(&path, loaded.clone());
        manager.persist_after_load(
            loaded,
            SettingsOrigin {
                recovered_unreadable: false,
                recovered_interrupted: false,
            },
            seeded,
        );

        assert!(path.exists(), "a missing canonical file must be written");
        let on_disk: AppSettings =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk.auto_delete_days, 365);
        let _ = fs::remove_dir_all(&dir);
    }

    /// A canonical file that parsed and needs no seed must not be rewritten.
    #[test]
    fn a_healthy_canonical_load_writes_nothing() {
        let dir =
            std::env::temp_dir().join(format!("cubby-settings-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let on_disk_bytes = "{\n  \"note\": \"written by hand\"\n}\n";
        fs::write(&path, on_disk_bytes).unwrap();

        let loaded = AppSettings {
            default_sensitive_apps_seeded: true,
            ..AppSettings::default()
        };
        let manager = manager_at(&path, loaded.clone());
        manager.persist_after_load(
            loaded,
            SettingsOrigin {
                recovered_unreadable: false,
                recovered_interrupted: false,
            },
            false,
        );

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            on_disk_bytes,
            "a healthy load must leave settings.json exactly as it found it"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// SBS-935: safe defaults turn retention off, which is right when nothing
    /// else is known and wrong when a file that still parses is sitting there.
    /// After the unreadable file has been quarantined, that file wins.
    #[test]
    fn a_parseable_file_beats_safe_defaults_after_a_quarantine() {
        let dir =
            std::env::temp_dir().join(format!("cubby-settings-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let legacy = dir.join("legacy-settings.json");
        let real = AppSettings {
            auto_delete_days: 365,
            default_sensitive_apps_seeded: true,
            ..AppSettings::default()
        };
        fs::write(&legacy, serde_json::to_string_pretty(&real).unwrap()).unwrap();

        let found = SettingsManager::settings_from_load_path(&legacy)
            .expect("a legacy file that parses must be used");
        assert_eq!(found.auto_delete_days, 365);
        assert_ne!(
            found.auto_delete_days,
            SettingsManager::safe_default_settings().auto_delete_days,
            "the user's real retention must win over safe-zero"
        );

        // Absent and unreadable both fall back to the safe defaults the caller
        // already holds.
        assert!(SettingsManager::settings_from_load_path(&dir.join("nothing.json")).is_none());
        let broken = dir.join("broken.json");
        fs::write(&broken, "{\"theme\": \"dark\", TRUNCATED").unwrap();
        assert!(SettingsManager::settings_from_load_path(&broken).is_none());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unreadable_interrupted_tmp_uses_safe_defaults_not_thirty_day() {
        let dir =
            std::env::temp_dir().join(format!("cubby-settings-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("settings.json");
        let tmp = settings_tmp_path(&path);
        fs::write(&tmp, "{\"theme\": \"dark\", TRUNCATED").unwrap();

        match SettingsManager::read_settings_from_disk(&path) {
            DiskRead::Settings {
                settings,
                recovered_unreadable,
                recovered_interrupted,
            } => {
                assert!(recovered_unreadable);
                assert!(recovered_interrupted);
                assert_eq!(settings.auto_delete_days, 0);
            }
            DiskRead::Missing => panic!("unreadable leftover temp is not a first-run miss"),
        }

        let _ = fs::remove_dir_all(&dir);
    }
}
