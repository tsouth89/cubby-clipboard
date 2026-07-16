use once_cell::sync::Lazy;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::{AppHandle, Manager};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

static CURRENT_SHORTCUT: Lazy<Mutex<Option<String>>> = Lazy::new(|| Mutex::new(None));
static REPLACE_WIN_V: AtomicBool = AtomicBool::new(false);

pub fn toggle_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        if window.is_visible().unwrap_or(false) && window.is_focused().unwrap_or(false) {
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

pub fn set_replace_win_v(enabled: bool) {
    REPLACE_WIN_V.store(enabled, Ordering::SeqCst);
    log::info!("SHORTCUT: Win+V replacement enabled={enabled}");
}

#[cfg(target_os = "windows")]
pub fn start_win_v_hook(app: AppHandle) -> Result<(), String> {
    use std::sync::mpsc;
    use std::time::Duration;
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::Input::KeyboardAndMouse::{VK_LWIN, VK_RWIN, VK_V};
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetMessageW, SetWindowsHookExW, TranslateMessage,
        UnhookWindowsHookEx, HC_ACTION, KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP,
        WM_SYSKEYDOWN, WM_SYSKEYUP,
    };

    static LEFT_WIN_DOWN: AtomicBool = AtomicBool::new(false);
    static RIGHT_WIN_DOWN: AtomicBool = AtomicBool::new(false);
    static V_DOWN: AtomicBool = AtomicBool::new(false);
    static TRIGGER: std::sync::OnceLock<mpsc::Sender<()>> = std::sync::OnceLock::new();

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code == HC_ACTION as i32 {
            let event = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            let key = event.vkCode;
            let message = wparam.0 as u32;
            let is_down = message == WM_KEYDOWN || message == WM_SYSKEYDOWN;
            let is_up = message == WM_KEYUP || message == WM_SYSKEYUP;

            if key == VK_LWIN.0 as u32 {
                LEFT_WIN_DOWN.store(is_down && !is_up, Ordering::SeqCst);
            } else if key == VK_RWIN.0 as u32 {
                RIGHT_WIN_DOWN.store(is_down && !is_up, Ordering::SeqCst);
            } else if key == VK_V.0 as u32 {
                if is_up && V_DOWN.swap(false, Ordering::SeqCst) {
                    return LRESULT(1);
                }

                if is_down
                    && REPLACE_WIN_V.load(Ordering::SeqCst)
                    && (LEFT_WIN_DOWN.load(Ordering::SeqCst)
                        || RIGHT_WIN_DOWN.load(Ordering::SeqCst))
                {
                    if !V_DOWN.swap(true, Ordering::SeqCst) {
                        if let Some(trigger) = TRIGGER.get() {
                            let _ = trigger.send(());
                        }
                    }
                    return LRESULT(1);
                }
            }
        }

        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }

    let (trigger_tx, trigger_rx) = mpsc::channel();
    TRIGGER
        .set(trigger_tx)
        .map_err(|_| "Win+V hook already initialized".to_string())?;

    std::thread::Builder::new()
        .name("cubby-win-v-actions".to_string())
        .spawn(move || {
            while trigger_rx.recv().is_ok() {
                toggle_main_window(&app);
            }
        })
        .map_err(|error| format!("Failed to start Win+V action worker: {error}"))?;

    let (ready_tx, ready_rx) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("cubby-win-v-hook".to_string())
        .spawn(move || unsafe {
            let hook = match SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0) {
                Ok(hook) => hook,
                Err(error) => {
                    let _ = ready_tx.send(Err(error.to_string()));
                    log::error!("SHORTCUT: Failed to install Win+V hook: {error}");
                    return;
                }
            };

            let _ = ready_tx.send(Ok(()));
            log::info!("SHORTCUT: Win+V keyboard hook installed");
            let mut message = MSG::default();
            while GetMessageW(&mut message, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            let _ = UnhookWindowsHookEx(hook);
        })
        .map_err(|error| format!("Failed to start Win+V hook thread: {error}"))?;

    ready_rx
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| "Timed out while installing the Win+V keyboard hook".to_string())?
        .map_err(|error| format!("Failed to install the Win+V keyboard hook: {error}"))
}

#[cfg(not(target_os = "windows"))]
pub fn start_win_v_hook(_app: AppHandle) -> Result<(), String> {
    Ok(())
}
