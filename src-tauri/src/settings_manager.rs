use crate::database::Database;
use crate::models::AppSettings;
use std::fs;
use std::path::PathBuf;
use std::sync::RwLock;
use tauri::AppHandle;
use tauri::Manager;

pub struct SettingsManager {
    file_path: PathBuf,
    settings: RwLock<AppSettings>,
}

impl SettingsManager {
    pub async fn new(app: &AppHandle, db: &Database) -> Self {
        // In portable mode settings live beside the executable with the rest of
        // the data; otherwise they stay in the per-user AppData directory.
        let base = crate::portable_data_dir().unwrap_or_else(|| app.path().app_data_dir().unwrap());
        let path = base.join("settings.json");
        let settings = if path.exists() {
            // Load from file
            match fs::read_to_string(&path) {
                Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
                Err(_) => AppSettings::default(),
            }
        } else {
            // Migrate from SQLite or use default
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
            let _ = manager.save(settings);
        }
        manager
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
