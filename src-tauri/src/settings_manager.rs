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

        let settings = if load_path.exists() {
            match fs::read_to_string(&load_path) {
                Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
                Err(_) => AppSettings::default(),
            }
        } else {
            // One-shot import from the old SQLite settings tables. After the
            // first successful JSON write, settings.json is the sole source.
            Self::migrate_from_sqlite(db).await
        };

        let mut settings = settings;
        let seeded_defaults = Self::seed_default_sensitive_apps(&mut settings);

        // Ensure we save it once immediately if migrating or seeding, so the
        // file exists and the one-time password-manager ignore list sticks.
        let manager = Self {
            file_path: path,
            settings: RwLock::new(settings.clone()),
        };
        if seeded_defaults || !manager.file_path.exists() {
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
        fs::write(&self.file_path, json).map_err(|e| e.to_string())?;
        {
            let mut lock = self.settings.write().unwrap();
            *lock = new_settings;
        }

        Ok(())
    }
}
