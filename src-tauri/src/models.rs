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
    pub remote_paste_mode: String,
    pub ignore_ghost_clips: bool,
    pub startup_with_windows: bool,
    pub round_corners: bool,
    pub float_above_taskbar: bool,
    pub density: String,

    // First-run onboarding: false until the user dismisses the welcome overlay.
    pub has_completed_onboarding: bool,

    // Privacy
    // Skip clipboard content that the source app tags as sensitive (password
    // managers, etc.). Matches Win+V, which also hides these. On by default.
    pub skip_sensitive: bool,
    // Skip text that matches high-confidence secret heuristics (tokens, keys,
    // payment cards). Off by default (opt-in): heuristic sniffing can drop a
    // clip the user meant to keep. Never logs matched content when enabled.
    pub skip_likely_secrets: bool,
    // True after the built-in password-manager ignore list has been seeded once.
    // Clearing an entry in Settings must stick across restarts.
    pub default_sensitive_apps_seeded: bool,
    pub ignored_apps: HashSet<String>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            theme: "system".to_string(),
            mica_effect: "solid".to_string(),
            language: "en".to_string(),
            // 0 = no item-count cap; retention is driven by age (auto_delete_days)
            // so "keep history for N days" means exactly that. Pinned items are
            // always kept regardless.
            max_items: 0,
            auto_delete_days: 30,
            hotkey: "Win+Alt+V".to_string(),
            replace_win_v: true,
            remote_paste_mode: "copy_then_paste".to_string(),
            ignore_ghost_clips: false,
            startup_with_windows: false,
            round_corners: true,
            float_above_taskbar: true,
            density: "comfortable".to_string(),

            has_completed_onboarding: false,

            skip_sensitive: true,
            // Heuristic content-sniffing is opt-in: guessing wrong silently drops
            // a clip the user deliberately copied, which erodes trust fast. The
            // reliable protections (OS sensitive tag + seeded password-manager
            // ignore list) stay on by default and cover the important cases.
            skip_likely_secrets: false,
            default_sensitive_apps_seeded: false,
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
    pub ocr_text: Option<String>,
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
            ocr_text: row.try_get("ocr_text")?,
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
    pub has_ocr_text: bool,
    pub ocr_match: Option<OcrMatch>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OcrMatch {
    pub before: String,
    pub matched: String,
    pub after: String,
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

    #[test]
    fn secret_privacy_defaults_are_opt_in() {
        // Omitted in an existing settings.json (upgrade path) and on a fresh
        // install, heuristic secret sniffing and seeding must both default off
        // so no clip is silently dropped and seeding runs at most once.
        let migrated: AppSettings = serde_json::from_str("{}")
            .expect("settings without the new fields should stay readable");
        assert!(!migrated.skip_likely_secrets);
        assert!(!migrated.default_sensitive_apps_seeded);

        let fresh = AppSettings::default();
        assert!(!fresh.skip_likely_secrets);
        assert!(!fresh.default_sensitive_apps_seeded);

        // The deterministic protection stays on regardless.
        assert!(fresh.skip_sensitive);
    }

    #[test]
    fn persisted_secret_privacy_values_survive_deserialization() {
        // A user who opts in, or a machine that has already seeded, must keep
        // those choices across restarts.
        let settings: AppSettings = serde_json::from_str(
            r#"{
                "skip_likely_secrets": true,
                "default_sensitive_apps_seeded": true
            }"#,
        )
        .expect("explicitly persisted privacy flags should round-trip");

        assert!(settings.skip_likely_secrets);
        assert!(settings.default_sensitive_apps_seeded);
    }
}
