use crate::settings_manager::SettingsManager;
use dark_light::Mode;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

#[tauri::command]
pub async fn get_settings(app: AppHandle) -> Result<serde_json::Value, String> {
    let manager = app.state::<Arc<SettingsManager>>();
    let settings = manager.get();
    let mut value = serde_json::to_value(&settings).map_err(|e| e.to_string())?;

    // Portable and Store builds deliberately omit registry autostart. Expose
    // capabilities explicitly so the frontend cannot offer controls that the
    // current distribution cannot honor.
    let portable = crate::portable_data_dir().is_some();
    let store_build = cfg!(feature = "app-store");
    let startup_available = !portable && !store_build;
    if let Some(obj) = value.as_object_mut() {
        obj.insert("is_portable".to_string(), serde_json::json!(portable));
        obj.insert(
            "startup_available".to_string(),
            serde_json::json!(startup_available),
        );
        // Same source as the plugin registration in lib.rs, so the reported
        // capability cannot claim an updater that was never installed.
        obj.insert(
            "self_update_available".to_string(),
            serde_json::json!(crate::self_update_supported()),
        );
        if !startup_available {
            obj.insert("startup_with_windows".to_string(), serde_json::json!(false));
            obj.insert(
                "startup_unavailable_reason".to_string(),
                serde_json::json!(if portable { "portable" } else { "app_store" }),
            );
        }
    }

    #[cfg(not(feature = "app-store"))]
    if !portable {
        use tauri_plugin_autostart::ManagerExt;
        match app.autolaunch().is_enabled() {
            Ok(is_enabled) => {
                if let Some(obj) = value.as_object_mut() {
                    obj.insert(
                        "startup_with_windows".to_string(),
                        serde_json::json!(is_enabled),
                    );
                }
            }
            Err(error) => {
                log::error!("SETTINGS: Could not read Windows startup state: {error}");
                if let Some(obj) = value.as_object_mut() {
                    obj.insert("startup_available".to_string(), serde_json::json!(false));
                    obj.insert("startup_with_windows".to_string(), serde_json::json!(false));
                    obj.insert(
                        "startup_unavailable_reason".to_string(),
                        serde_json::json!("error"),
                    );
                }
            }
        }
    }

    Ok(value)
}

#[tauri::command]
pub async fn save_settings(
    app: AppHandle,
    settings: serde_json::Value,
    changed_keys: Option<Vec<String>>,
) -> Result<(), String> {
    let manager = app.state::<Arc<SettingsManager>>();

    let current = manager.get();
    let portable = crate::portable_data_dir().is_some();
    let app_store_build = cfg!(feature = "app-store");

    let (mut new_settings, startup_setting_changed) = prepare_settings_for_save(
        settings,
        changed_keys.as_deref(),
        &current,
        portable,
        app_store_build,
    )?;

    #[cfg(not(feature = "app-store"))]
    let autostart_transition = if portable || !startup_setting_changed {
        None
    } else {
        use tauri_plugin_autostart::ManagerExt;
        let active = app
            .autolaunch()
            .is_enabled()
            .map_err(|error| format!("Could not read Windows startup state: {error}"))?;
        (active != new_settings.startup_with_windows)
            .then_some((active, new_settings.startup_with_windows))
    };

    let shortcut_settings_changed = new_settings.hotkey != current.hotkey
        || new_settings.replace_win_v != current.replace_win_v;
    if shortcut_settings_changed {
        crate::shortcuts::register_shortcuts(
            &app,
            &new_settings.hotkey,
            new_settings.replace_win_v,
        )?;
    }

    // Reconfigure the helper whenever the hotkey or the replacement toggle
    // changes, so the remote-session trigger tracks the current hotkey.
    if shortcut_settings_changed {
        let replacement = app.state::<Arc<crate::win_v_replacement::WinVReplacementManager>>();
        // replace_win_v defaults to true and persists, so a hotkey-only save on
        // a session whose shortcut channel failed to bind still arrives here as
        // enabled=true. Rolling the save back for that left the user unable to
        // change their hotkey until they switched replacement off, and that off
        // value would then stick on the next good launch (SBS-991).
        let newly_enabled = new_settings.replace_win_v && !current.replace_win_v;
        if crate::win_v_replacement::shortcut_save_blocked(
            replacement.is_available(),
            newly_enabled,
        ) {
            restore_shortcut_settings(&app, &current);
            return Err("Cubby shortcut channel is unavailable this session".to_string());
        }
        if replacement.is_available() {
            if let Err(error) = replacement.configure(
                new_settings.replace_win_v,
                Some(new_settings.hotkey.clone()),
            ) {
                restore_shortcut_settings(&app, &current);
                return Err(error);
            }
        } else {
            log::warn!(
                "SETTINGS: Saved shortcut settings without the Win+V helper; the shortcut channel is unavailable this session"
            );
        }
    }

    #[cfg(not(feature = "app-store"))]
    if let Some((_, requested)) = autostart_transition {
        if let Err(error) = set_autostart(&app, requested) {
            if shortcut_settings_changed {
                restore_shortcut_settings(&app, &current);
            }
            return Err(error);
        }
    }

    // Persist the selection before applying non-critical visual side effects.
    // This keeps the UI and settings file consistent even if Windows rejects
    // a backdrop on the current system or window state.
    if let Err(error) = manager.save(new_settings.clone()) {
        if shortcut_settings_changed {
            restore_shortcut_settings(&app, &current);
        }
        #[cfg(not(feature = "app-store"))]
        if let Some((previous, _)) = autostart_transition {
            if let Err(rollback_error) = set_autostart(&app, previous) {
                log::error!(
                    "SETTINGS: Could not restore Windows startup state after save failure: {rollback_error}"
                );
            }
        }
        return Err(error);
    }

    // Window effect
    let theme_str = new_settings.theme.clone();
    let mica_effect = new_settings.mica_effect.clone();
    let round_corners = new_settings.round_corners;
    log::info!(
        "save_settings: mica_effect={}, theme={}",
        mica_effect,
        theme_str
    );
    match app.get_webview_window("main") {
        Some(win) => {
            let current_theme = if theme_str == "light" {
                tauri::Theme::Light
            } else if theme_str == "dark" {
                tauri::Theme::Dark
            } else {
                match dark_light::detect() {
                    Ok(Mode::Dark) => tauri::Theme::Dark,
                    Ok(_) => tauri::Theme::Light,
                    Err(error) => {
                        log::warn!(
                            "save_settings: system theme detection failed, using window theme: {:?}",
                            error
                        );
                        win.theme().unwrap_or(tauri::Theme::Dark)
                    }
                }
            };
            crate::apply_window_effect(&win, &mica_effect, &current_theme, round_corners);
        }
        None => {
            log::warn!("save_settings: main window not found, skipping window effect");
        }
    }

    log::info!(
        "save_settings: language={}, theme={}",
        new_settings.language,
        new_settings.theme
    );
    Ok(())
}

fn restore_shortcut_settings(app: &AppHandle, settings: &crate::models::AppSettings) {
    if let Err(error) =
        crate::shortcuts::register_shortcuts(app, &settings.hotkey, settings.replace_win_v)
    {
        log::error!("SETTINGS: Could not restore shortcut configuration: {error}");
    }
    let replacement = app.state::<Arc<crate::win_v_replacement::WinVReplacementManager>>();
    if let Err(error) = replacement.configure(settings.replace_win_v, Some(settings.hotkey.clone()))
    {
        log::error!("SETTINGS: Could not restore Win+V helper configuration: {error}");
    }
}

#[cfg(not(feature = "app-store"))]
fn set_autostart(app: &AppHandle, enabled: bool) -> Result<(), String> {
    use tauri_plugin_autostart::ManagerExt;

    let result = if enabled {
        app.autolaunch().enable()
    } else {
        app.autolaunch().disable()
    };
    result.map_err(|error| {
        format!(
            "Could not {} startup with Windows: {error}",
            if enabled { "enable" } else { "disable" }
        )
    })
}

#[tauri::command]
pub async fn complete_onboarding(app: AppHandle) -> Result<(), String> {
    let manager = app.state::<Arc<SettingsManager>>();
    let mut current = manager.get();
    if !current.has_completed_onboarding {
        current.has_completed_onboarding = true;
        manager.save(current)?;
    }
    Ok(())
}

/// Trims and validates before inserting. Returns whether the set actually changed, so the
/// caller only persists when there is something new to save.
fn apply_add_ignored_app(app_name: &str, current: &mut crate::models::AppSettings) -> bool {
    let trimmed = app_name.trim();
    if trimmed.is_empty() {
        return false;
    }
    current.ignored_apps.insert(trimmed.to_string())
}

/// Mirrors `apply_add_ignored_app`'s trimming so "  testapp  " removes the same entry
/// "testapp" added.
fn apply_remove_ignored_app(app_name: &str, current: &mut crate::models::AppSettings) -> bool {
    let trimmed = app_name.trim();
    if trimmed.is_empty() {
        return false;
    }
    current.ignored_apps.remove(trimmed)
}

#[tauri::command]
pub async fn add_ignored_app(app_name: String, app: AppHandle) -> Result<(), String> {
    let manager = app.state::<Arc<SettingsManager>>();
    let mut current = manager.get();
    if apply_add_ignored_app(&app_name, &mut current) {
        manager.save(current)?;
    }
    Ok(())
}

#[tauri::command]
pub async fn remove_ignored_app(app_name: String, app: AppHandle) -> Result<(), String> {
    let manager = app.state::<Arc<SettingsManager>>();
    let mut current = manager.get();
    if apply_remove_ignored_app(&app_name, &mut current) {
        manager.save(current)?;
    }
    Ok(())
}

#[tauri::command]
pub async fn get_ignored_apps(app: AppHandle) -> Result<Vec<String>, String> {
    let manager = app.state::<Arc<SettingsManager>>();
    let mut apps: Vec<String> = manager.get().ignored_apps.into_iter().collect();
    apps.sort();
    Ok(apps)
}

fn prepare_settings_for_save(
    settings: serde_json::Value,
    changed_keys: Option<&[String]>,
    current: &crate::models::AppSettings,
    portable: bool,
    app_store_build: bool,
) -> Result<(crate::models::AppSettings, bool), String> {
    let mut new_settings: crate::models::AppSettings =
        serde_json::from_value(settings).map_err(|e| e.to_string())?;

    new_settings.ignored_apps = current.ignored_apps.clone();
    new_settings.default_sensitive_apps_seeded = current.default_sensitive_apps_seeded;

    let startup_setting_changed = changed_keys
        .map(|keys| keys.iter().any(|key| key == "startup_with_windows"))
        .unwrap_or(new_settings.startup_with_windows != current.startup_with_windows);

    if portable || app_store_build {
        new_settings.startup_with_windows = false;
    } else if !startup_setting_changed {
        new_settings.startup_with_windows = current.startup_with_windows;
    }

    Ok((new_settings, startup_setting_changed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::AppSettings;

    #[test]
    fn test_ignored_apps_add() {
        let mut current = AppSettings::default();

        assert!(apply_add_ignored_app("testapp", &mut current));
        assert!(current.ignored_apps.contains("testapp"));

        // Duplicate
        assert!(!apply_add_ignored_app("testapp", &mut current));

        // Whitespace and empty
        assert!(!apply_add_ignored_app("   ", &mut current));
        assert!(!apply_add_ignored_app("", &mut current));

        // Trimming
        assert!(apply_add_ignored_app("  app2  ", &mut current));
        assert!(current.ignored_apps.contains("app2"));
    }

    #[test]
    fn test_ignored_apps_remove() {
        let mut current = AppSettings::default();
        current.ignored_apps.insert("testapp".to_string());

        assert!(apply_remove_ignored_app("testapp", &mut current));
        assert!(!current.ignored_apps.contains("testapp"));

        // Does not exist
        assert!(!apply_remove_ignored_app("testapp", &mut current));

        // Whitespace and empty
        assert!(!apply_remove_ignored_app("   ", &mut current));
        assert!(!apply_remove_ignored_app("", &mut current));
    }

    #[test]
    fn test_prepare_settings_for_save_roundtrip() {
        let current = AppSettings::default();
        let new_settings = AppSettings {
            theme: "dark".to_string(),
            startup_with_windows: true,
            ..AppSettings::default()
        };

        let json = serde_json::to_value(&new_settings).unwrap();

        // Normal save
        let (saved, changed) =
            prepare_settings_for_save(json.clone(), None, &current, false, false).unwrap();
        assert_eq!(saved.theme, "dark");
        assert!(saved.startup_with_windows);
        assert!(changed);

        // Portable prevents startup
        let (saved_port, _) =
            prepare_settings_for_save(json.clone(), None, &current, true, false).unwrap();
        assert!(!saved_port.startup_with_windows);

        // Store build prevents startup
        let (saved_store, _) =
            prepare_settings_for_save(json.clone(), None, &current, false, true).unwrap();
        assert!(!saved_store.startup_with_windows);
    }

    #[test]
    fn test_prepare_settings_for_save_preserves_server_owned_privacy_state() {
        // The frontend does not round-trip ignored_apps or the seed flag, so a save must not
        // let their absence from the incoming JSON erase them -- a missing/false seed flag
        // would re-insert default password managers on the next startup.
        let mut current = AppSettings::default();
        current.ignored_apps.insert("keepass".to_string());
        current.default_sensitive_apps_seeded = true;

        // The frontend's own default omits both fields entirely.
        let incoming = AppSettings::default();
        let json = serde_json::to_value(&incoming).unwrap();

        let (saved, _) = prepare_settings_for_save(json, None, &current, false, false).unwrap();
        assert!(saved.ignored_apps.contains("keepass"));
        assert!(saved.default_sensitive_apps_seeded);
    }
}
