use crate::database::Database;
use crate::models::AppSettings;
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

impl SettingsManager {
    pub async fn new(app: &AppHandle, db: &Database) -> Self {
        // Keep settings on the same data root as the database (including the
        // debug `/dev` isolation from SOU-227 and portable mode).
        let base = crate::get_data_dir();
        let path = base.join("settings.json");
        let load_path = Self::resolve_settings_load_path(app, &base, &path);

        let (settings, recovered_unreadable) = if load_path.exists() {
            match fs::read_to_string(&load_path)
                .map_err(|e| e.to_string())
                .and_then(|content| serde_json::from_str(&content).map_err(|e| e.to_string()))
            {
                Ok(settings) => (settings, false),
                Err(error) => (
                    Self::recover_from_unreadable_settings(&load_path, &error),
                    true,
                ),
            }
        } else {
            // One-shot import from the old SQLite settings tables. After the
            // first successful JSON write, settings.json is the sole source.
            (Self::migrate_from_sqlite(db).await, false)
        };

        let mut settings = settings;
        let seeded_defaults = Self::seed_default_sensitive_apps(&mut settings);

        // Ensure we save it once immediately if migrating or seeding, so the
        // file exists and the one-time password-manager ignore list sticks.
        let manager = Self {
            file_path: path,
            settings: RwLock::new(settings.clone()),
        };
        if seeded_defaults || recovered_unreadable || !manager.file_path.exists() {
            if let Err(error) = manager.save(settings) {
                log::error!("SETTINGS: Failed to persist settings: {error}");
            }
        }
        manager
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
        if let Some(v) = get_val(pool, "language").await {
            settings.language = v;
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
        let tmp_path = self.file_path.with_extension("json.tmp");
        fs::write(&tmp_path, json).map_err(|e| e.to_string())?;
        if let Err(error) = replace_file_atomically(&tmp_path, &self.file_path) {
            let _ = fs::remove_file(&tmp_path);
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
}
