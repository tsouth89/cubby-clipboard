use crate::settings_manager::SettingsManager;
use dark_light::Mode;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

#[tauri::command]
pub async fn get_settings(app: AppHandle) -> Result<serde_json::Value, String> {
    let manager = app.state::<Arc<SettingsManager>>();
    let settings = manager.get();
    let mut value = serde_json::to_value(&settings).map_err(|e| e.to_string())?;

    #[cfg(not(feature = "app-store"))]
    {
        use tauri_plugin_autostart::ManagerExt;
        if let Ok(is_enabled) = app.autolaunch().is_enabled() {
            if let Some(obj) = value.as_object_mut() {
                obj.insert(
                    "startup_with_windows".to_string(),
                    serde_json::json!(is_enabled),
                );
            }
        }
    }

    Ok(value)
}

#[tauri::command]
pub async fn save_settings(app: AppHandle, settings: serde_json::Value) -> Result<(), String> {
    let manager = app.state::<Arc<SettingsManager>>();

    // Deserialize incoming settings (Frontend sends full object except ignored_apps)
    let mut new_settings: crate::models::AppSettings =
        serde_json::from_value(settings).map_err(|e| e.to_string())?;

    // Preserve ignored_apps from current state (as frontend doesn't send it in this call)
    let current = manager.get();
    new_settings.ignored_apps = current.ignored_apps.clone();

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
            let _ =
                crate::shortcuts::register_shortcuts(&app, &current.hotkey, current.replace_win_v);
            let _ = replacement.configure(current.replace_win_v, Some(current.hotkey.clone()));
            return Err(error);
        }
    }

    // Persist the selection before applying non-critical visual side effects.
    // This keeps the UI and settings file consistent even if Windows rejects
    // a backdrop on the current system or window state.
    if let Err(error) = manager.save(new_settings.clone()) {
        if shortcut_settings_changed {
            let _ =
                crate::shortcuts::register_shortcuts(&app, &current.hotkey, current.replace_win_v);
            let replacement = app.state::<Arc<crate::win_v_replacement::WinVReplacementManager>>();
            let _ = replacement.configure(current.replace_win_v, Some(current.hotkey.clone()));
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

    #[cfg(not(feature = "app-store"))]
    {
        use tauri_plugin_autostart::ManagerExt;
        // Check if startup changed
        let startup = new_settings.startup_with_windows;
        let current_state = app.autolaunch().is_enabled().unwrap_or(false);
        if startup != current_state {
            if startup {
                let _ = app.autolaunch().enable();
            } else {
                let _ = app.autolaunch().disable();
            }
        }
    }
    log::info!(
        "save_settings: auto_paste={}, language={}, theme={}",
        new_settings.auto_paste,
        new_settings.language,
        new_settings.theme
    );
    Ok(())
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
