use once_cell::sync::Lazy;
use std::str::FromStr;
use std::sync::Mutex;
use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

static CURRENT_SHORTCUT: Lazy<Mutex<Option<String>>> = Lazy::new(|| Mutex::new(None));

pub fn toggle_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        if window.is_visible().unwrap_or(false) {
            crate::animate_window_hide(&window, None);
        } else {
            crate::position_window_at_bottom(&window);
        }
    }
}

pub fn register_standard_shortcut(app: &AppHandle, hotkey: &str) -> Result<(), String> {
    let shortcut =
        Shortcut::from_str(hotkey).map_err(|error| format!("Invalid shortcut: {error:?}"))?;
    let previous = CURRENT_SHORTCUT.lock().unwrap().clone();
    if previous.as_deref() == Some(hotkey) {
        return Ok(());
    }

    app.global_shortcut()
        .unregister_all()
        .map_err(|error| format!("Could not release the previous shortcut: {error:?}"))?;

    let app_for_handler = app.clone();
    let registration =
        app.global_shortcut()
            .on_shortcut(shortcut, move |_app, _shortcut, event| {
                if event.state() == ShortcutState::Pressed {
                    toggle_main_window(&app_for_handler);
                }
            });

    if let Err(error) = registration {
        if let Some(previous) = previous {
            if previous != hotkey {
                if let Err(restore_error) = register_without_fallback(app, &previous) {
                    log::error!(
                        "SHORTCUT: Failed to restore {} after conflict: {}",
                        previous,
                        restore_error
                    );
                }
            }
        }
        return Err(format!(
            "{hotkey} is unavailable. Another application or Windows may already be using it. ({error:?})"
        ));
    }

    *CURRENT_SHORTCUT.lock().unwrap() = Some(hotkey.to_string());
    log::info!("SHORTCUT: Registered {}", hotkey);
    Ok(())
}

fn register_without_fallback(app: &AppHandle, hotkey: &str) -> Result<(), String> {
    let shortcut =
        Shortcut::from_str(hotkey).map_err(|error| format!("Invalid shortcut: {error:?}"))?;
    let app_for_handler = app.clone();
    app.global_shortcut()
        .on_shortcut(shortcut, move |_app, _shortcut, event| {
            if event.state() == ShortcutState::Pressed {
                toggle_main_window(&app_for_handler);
            }
        })
        .map_err(|error| format!("Failed to register shortcut: {error:?}"))?;
    *CURRENT_SHORTCUT.lock().unwrap() = Some(hotkey.to_string());
    Ok(())
}
