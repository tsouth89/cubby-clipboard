use serde::{Deserialize, Serialize};
use sqlx::{sqlite::SqliteRow, FromRow, Row};
use std::sync::OnceLock;

use std::collections::HashSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub theme: String,
    pub mica_effect: String,
    pub language: String,
    pub max_items: i64,
    pub auto_delete_days: i64,
    pub hotkey: String,
    pub replace_win_v: bool,
    pub auto_paste: bool,
    pub remote_paste_mode: String,
    pub ignore_ghost_clips: bool,
    pub startup_with_windows: bool,
    pub round_corners: bool,
    pub float_above_taskbar: bool,
    pub density: String,

    // Privacy
    pub ignored_apps: HashSet<String>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            theme: "system".to_string(),
            mica_effect: "solid".to_string(),
            language: "en".to_string(),
            max_items: 1000,
            auto_delete_days: 30,
            hotkey: "Win+Alt+V".to_string(),
            replace_win_v: true,
            auto_paste: false,
            remote_paste_mode: "copy_then_paste".to_string(),
            ignore_ghost_clips: false,
            startup_with_windows: false,
            round_corners: true,
            float_above_taskbar: true,
            density: "comfortable".to_string(),

            ignored_apps: HashSet::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Clip {
    pub id: i64,
    pub uuid: String,
    pub clip_type: String,
    pub content: Vec<u8>,
    pub text_preview: String,
    pub content_hash: String,
    pub folder_id: Option<i64>,
    pub is_deleted: bool,
    pub is_pinned: bool,
    pub is_thumbnail: bool,
    pub source_app: Option<String>,
    pub source_icon: Option<String>,
    pub metadata: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_accessed: chrono::DateTime<chrono::Utc>,
}

impl<'r> FromRow<'r, SqliteRow> for Clip {
    fn from_row(row: &'r SqliteRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            uuid: row.try_get("uuid")?,
            clip_type: row.try_get("clip_type")?,
            content: row.try_get("content")?,
            text_preview: row.try_get("text_preview")?,
            content_hash: row.try_get("content_hash")?,
            folder_id: row.try_get("folder_id")?,
            is_deleted: row.try_get("is_deleted")?,
            is_pinned: row.try_get("is_pinned")?,
            is_thumbnail: row.try_get("is_thumbnail")?,
            source_app: row.try_get("source_app")?,
            source_icon: row.try_get("source_icon")?,
            metadata: row.try_get("metadata")?,
            created_at: row.try_get("created_at")?,
            last_accessed: row.try_get("last_accessed")?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Folder {
    pub id: i64,
    pub name: String,
    pub icon: Option<String>,
    pub color: Option<String>,
    pub is_system: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl<'r> FromRow<'r, SqliteRow> for Folder {
    fn from_row(row: &'r SqliteRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: row.try_get("id")?,
            name: row.try_get("name")?,
            icon: row.try_get("icon")?,
            color: row.try_get("color")?,
            is_system: row.try_get("is_system")?,
            created_at: row.try_get("created_at")?,
        })
    }
}

static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

pub fn get_runtime() -> Result<&'static tokio::runtime::Runtime, String> {
    if let Some(rt) = RUNTIME.get() {
        return Ok(rt);
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;

    RUNTIME.set(rt).ok();
    Ok(RUNTIME.get().unwrap())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardItem {
    pub id: String,
    pub clip_type: String,
    pub content: String,
    pub preview: String,
    pub folder_id: Option<String>,
    pub is_pinned: bool,
    pub created_at: String,
    pub source_app: Option<String>,
    pub source_icon: Option<String>,
    pub metadata: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderItem {
    pub id: String,
    pub name: String,
    pub icon: Option<String>,
    pub color: Option<String>,
    pub is_system: bool,
    pub item_count: i64,
}

#[cfg(test)]
mod tests {
    use super::AppSettings;

    #[test]
    fn existing_settings_keep_their_configured_shortcut() {
        let settings: AppSettings = serde_json::from_str(
            r#"{
                "theme": "system",
                "hotkey": "Ctrl+Shift+V"
            }"#,
        )
        .expect("existing settings should remain readable");

        assert_eq!(settings.hotkey, "Ctrl+Shift+V");
        assert!(settings.replace_win_v);
        assert_eq!(settings.remote_paste_mode, "copy_then_paste");
        assert_eq!(settings.density, "comfortable");
    }

    #[test]
    fn new_settings_use_win_alt_v() {
        let settings = AppSettings::default();

        assert_eq!(settings.hotkey, "Win+Alt+V");
        assert!(settings.replace_win_v);
        assert_eq!(settings.remote_paste_mode, "copy_then_paste");
        assert_eq!(settings.density, "comfortable");
    }
}
