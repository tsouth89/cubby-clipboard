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

    // Deserialize incoming settings (Frontend sends full object except ignored_apps)
    let mut new_settings: crate::models::AppSettings =
        serde_json::from_value(settings).map_err(|e| e.to_string())?;

    // Preserve server-owned privacy list state. The frontend does not round-trip
    // ignored apps or the one-time seed flag; trusting a missing/false seed flag
    // would re-insert default password managers on the next startup.
    let current = manager.get();
    new_settings.ignored_apps = current.ignored_apps.clone();
    new_settings.default_sensitive_apps_seeded = current.default_sensitive_apps_seeded;

    // Newer frontends identify the exact fields involved in this save. Keep a
    // value comparison fallback for older callers, but avoid touching Windows
    // startup state for unrelated settings when the changed fields are known.
    let startup_setting_changed = changed_keys
        .as_ref()
        .map(|keys| keys.iter().any(|key| key == "startup_with_windows"))
        .unwrap_or(new_settings.startup_with_windows != current.startup_with_windows);

    let portable = crate::portable_data_dir().is_some();
    if portable || cfg!(feature = "app-store") {
        // Never persist a capability the current build cannot apply. This also
        // clears a stale installed-build preference after switching channels.
        new_settings.startup_with_windows = false;
    } else if !startup_setting_changed {
        // The OS is authoritative for display, but the persisted fallback must
        // not be rewritten from an unrelated save or an unavailable read.
        new_settings.startup_with_windows = current.startup_with_windows;
    }

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
        if let Err(error) = replacement.configure(
            new_settings.replace_win_v,
            Some(new_settings.hotkey.clone()),
        ) {
            restore_shortcut_settings(&app, &current);
            return Err(error);
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

#[tauri::command]
pub async fn add_ignored_app(app_name: String, app: AppHandle) -> Result<(), String> {
    let manager = app.state::<Arc<SettingsManager>>();
    let mut current = manager.get();
    if current.ignored_apps.insert(app_name) {
        manager.save(current)?;
    }
    Ok(())
}

#[tauri::command]
pub async fn remove_ignored_app(app_name: String, app: AppHandle) -> Result<(), String> {
    let manager = app.state::<Arc<SettingsManager>>();
    let mut current = manager.get();
    if current.ignored_apps.remove(&app_name) {
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
