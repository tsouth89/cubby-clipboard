use once_cell::sync::Lazy;
use std::str::FromStr;
use std::sync::Mutex;
use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

pub const WIN_V_BRIDGE_SHORTCUT: &str = "Win+Alt+V";

#[derive(Clone, Default, PartialEq, Eq)]
struct ShortcutConfiguration {
    hotkey: Option<String>,
    replace_win_v: bool,
}

static CURRENT_CONFIGURATION: Lazy<Mutex<ShortcutConfiguration>> =
    Lazy::new(|| Mutex::new(ShortcutConfiguration::default()));

fn parser_hotkey(hotkey: &str) -> String {
    hotkey
        .split('+')
        .map(|token| {
            let trimmed = token.trim();
            if trimmed.eq_ignore_ascii_case("win") || trimmed.eq_ignore_ascii_case("windows") {
                "Super"
            } else {
                trimmed
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

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
    let replace_win_v = CURRENT_CONFIGURATION.lock().unwrap().replace_win_v;
    register_shortcuts(app, hotkey, replace_win_v)
}

pub fn register_shortcuts(
    app: &AppHandle,
    hotkey: &str,
    replace_win_v: bool,
) -> Result<(), String> {
    let parser_value = parser_hotkey(hotkey);
    let shortcut = Shortcut::from_str(&parser_value)
        .map_err(|error| format!("Invalid shortcut: {error:?}"))?;
    let bridge = if replace_win_v && !hotkey.eq_ignore_ascii_case(WIN_V_BRIDGE_SHORTCUT) {
        Some(
            Shortcut::from_str(&parser_hotkey(WIN_V_BRIDGE_SHORTCUT))
                .map_err(|error| format!("Invalid Win+V bridge shortcut: {error:?}"))?,
        )
    } else {
        None
    };
    let requested = ShortcutConfiguration {
        hotkey: Some(hotkey.to_string()),
        replace_win_v,
    };
    let previous = CURRENT_CONFIGURATION.lock().unwrap().clone();
    if previous == requested {
        return Ok(());
    }

    app.global_shortcut()
        .unregister_all()
        .map_err(|error| format!("Could not release the previous shortcut: {error:?}"))?;

    let registration = register_one(app, shortcut)
        .map_err(|error| format!("{hotkey}: {error}"))
        .and_then(|_| {
            if let Some(bridge) = bridge {
                register_one(app, bridge)
                    .map_err(|error| format!("{WIN_V_BRIDGE_SHORTCUT}: {error}"))
            } else {
                Ok(())
            }
        });

    if let Err(error) = registration {
        let _ = app.global_shortcut().unregister_all();
        if let Err(restore_error) = register_configuration_without_fallback(app, &previous) {
            log::error!(
                "SHORTCUT: Failed to restore previous configuration after conflict: {}",
                restore_error
            );
        } else {
            *CURRENT_CONFIGURATION.lock().unwrap() = previous;
        }
        let unavailable = if error.contains(WIN_V_BRIDGE_SHORTCUT) {
            WIN_V_BRIDGE_SHORTCUT
        } else {
            hotkey
        };
        return Err(format!(
            "{unavailable} is unavailable. Another application or Windows may already be using it. ({error})"
        ));
    }

    *CURRENT_CONFIGURATION.lock().unwrap() = requested;
    if replace_win_v {
        log::info!(
            "SHORTCUT: Registered {} with Win+V replacement bridge {}",
            hotkey,
            WIN_V_BRIDGE_SHORTCUT
        );
    } else {
        log::info!("SHORTCUT: Registered {}", hotkey);
    }
    Ok(())
}

fn register_one(app: &AppHandle, shortcut: Shortcut) -> Result<(), String> {
    let app_for_handler = app.clone();
    app.global_shortcut()
        .on_shortcut(shortcut, move |_app, _shortcut, event| {
            if event.state() == ShortcutState::Pressed {
                toggle_main_window(&app_for_handler);
            }
        })
        .map_err(|error| format!("Failed to register shortcut: {error:?}"))
}

fn register_configuration_without_fallback(
    app: &AppHandle,
    configuration: &ShortcutConfiguration,
) -> Result<(), String> {
    let Some(hotkey) = configuration.hotkey.as_deref() else {
        return Ok(());
    };
    let shortcut = Shortcut::from_str(&parser_hotkey(hotkey))
        .map_err(|error| format!("Invalid shortcut: {error:?}"))?;
    register_one(app, shortcut)?;
    if configuration.replace_win_v && !hotkey.eq_ignore_ascii_case(WIN_V_BRIDGE_SHORTCUT) {
        let bridge = Shortcut::from_str(&parser_hotkey(WIN_V_BRIDGE_SHORTCUT))
            .map_err(|error| format!("Invalid Win+V bridge shortcut: {error:?}"))?;
        register_one(app, bridge)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parser_hotkey;

    #[test]
    fn normalizes_windows_friendly_modifier_names() {
        assert_eq!(parser_hotkey("Win+Alt+V"), "Super+Alt+V");
        assert_eq!(parser_hotkey("Windows+Ctrl+Alt+V"), "Super+Ctrl+Alt+V");
    }

    #[test]
    fn preserves_other_shortcut_tokens() {
        assert_eq!(parser_hotkey("Ctrl+Shift+V"), "Ctrl+Shift+V");
    }
}
