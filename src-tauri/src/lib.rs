use std::fs;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use tauri::{
    image::Image,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{TrayIcon, TrayIconBuilder},
    Manager,
};
#[cfg(not(feature = "app-store"))]
use tauri_plugin_autostart::MacosLauncher;

static IS_ANIMATING: AtomicBool = AtomicBool::new(false);
static LAST_SHOW_TIME: AtomicI64 = AtomicI64::new(0);

mod clipboard;
mod commands;
mod constants;
mod database;
mod models;
mod settings_commands;
mod settings_manager;
mod shortcuts;
mod win_v_replacement;

use database::Database;
use models::get_runtime;
use settings_manager::SettingsManager;

pub fn run_app() {
    let data_dir = get_data_dir();
    fs::create_dir_all(&data_dir).ok();
    let db_path = data_dir.join("cubby.db");
    let db_path_str = db_path.to_str().unwrap_or("cubby.db").to_string();

    let rt = get_runtime().expect("Failed to get global tokio runtime");
    let _guard = rt.enter();

    let db = rt.block_on(async { Database::new(&db_path_str).await });

    rt.block_on(async {
        db.migrate().await.ok();
    });

    let db_arc = Arc::new(db);

    let mut log_builder = tauri_plugin_log::Builder::default()
        .format(|out, message, record| {
            out.finish(format_args!(
                "[{}][{}][{}] {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"),
                record.target(),
                record.level(),
                message
            ))
        })
        .level(log::LevelFilter::Debug)
        .level_for("sqlx", log::LevelFilter::Warn);

    #[cfg(debug_assertions)]
    {
        log_builder = log_builder.targets([
            tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout),
            tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Webview),
        ]);
    }

    #[cfg(not(debug_assertions))]
    {
        log_builder = log_builder.targets([
            tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::LogDir { file_name: None }),
            tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Webview),
        ]);
    }

    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default();

    #[cfg(not(feature = "app-store"))]
    {
        builder = builder.plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--flag1", "--flag2"]),
        ));
    }

    builder
        .plugin(log_builder.build())
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            log::info!("Second instance detected. Sending notification and exiting.");
            use tauri_plugin_notification::NotificationExt;
            if let Err(e) = app.notification()
                .builder()
                .title("Cubby")
                .body("Cubby is already running")
                .show() {
                log::error!("Failed to send notification: {:?}", e);
            }
        }))
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_x::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_opener::init())
        .manage(db_arc.clone())
        .on_window_event(|window, event| {
            match event {
                tauri::WindowEvent::ThemeChanged(theme) => {
                    log::info!("THEME:System theme changed to: {:?}, win.theme(): {:?}", theme, window.theme());
                    let label = window.label().to_string();
                    let app_handle = window.app_handle().clone();
                    let theme_ = *theme;

                    // Update tray icon to match new system theme
                    if let Some(tray) = app_handle.tray_by_id("main") {
                        update_tray_icon(&tray, &theme_);
                    }

                    // Use SettingsManager
                    let manager = window.state::<Arc<SettingsManager>>();
                    let settings = manager.get();

                    tauri::async_runtime::spawn(async move {
                        let current_theme = settings.theme;
                        let mica_effect = settings.mica_effect;
                        let round_corners = settings.round_corners;

                        log::info!("THEME:Re-applying window effect due to theme change. Current theme setting: {:?}, system theme: {:?}, mica_effect setting: {:?}", current_theme, theme_, mica_effect);
                        // If app is set to follow system, we re-apply based on the NEW system theme
                        if current_theme == "system" {
                            if let Some(webview_win) = app_handle.get_webview_window(&label) {
                                crate::apply_window_effect(&webview_win, &mica_effect, &theme_, round_corners);
                            }
                        }
                    });
                }
                tauri::WindowEvent::Focused(false) => {
                    let label = window.label();
                    // Only auto-hide the main window
                    if label != "main" {
                        return;
                    }
                    if window.app_handle().get_webview_window("settings").is_some() {
                        // Settings window is open, keep main window visible
                        return;
                    }

                    // Debounce: Ignore blur events immediately after showing
                    let last_show = LAST_SHOW_TIME.load(Ordering::SeqCst);
                    let now = chrono::Local::now().timestamp_millis();
                    let debounce_ms = 500;
                    if now - last_show < debounce_ms {
                        return;
                    }

                    if let Some(win) = window.app_handle().get_webview_window(label) {
                        // Safety checks:
                        // 1. If we are already animating (e.g. hiding via hotkey), don't interfere.
                        if IS_ANIMATING.load(Ordering::SeqCst) {
                            return;
                        }
                        // 2. If the window is not visible (e.g. just hidden programmatically), don't try to move/show it.
                        if !win.is_visible().unwrap_or(false) {
                            return;
                        }

                        // Check if cursor is on a different monitor
                        let current_monitor = win.current_monitor().ok().flatten();
                        let cursor_monitor = get_monitor_at_cursor(&win);
                        let moved_screens =
                            if let (Some(cm), Some(crm)) = (&current_monitor, &cursor_monitor) {
                                cm.position().x != crm.position().x
                                    || cm.position().y != crm.position().y
                            } else {
                                false
                            };

                        if moved_screens {
                            // User clicked on another screen, move window there immediately
                            position_window_near_cursor(&win);
                            let _ = win.show();
                            let _ = win.set_focus();
                        } else {
                            // Normal blur handling (hide)
                            let win_clone = win.clone();
                            std::thread::spawn(move || {
                                crate::animate_window_hide(&win_clone, None);
                            });
                        }
                    }
                }
                _ => {}
            }
        })
        .setup(move |app| {
            log::info!("Cubby starting...");

            // Initialize Settings Manager
            let db_for_settings = db_arc.clone();
            let settings_manager = get_runtime().unwrap().block_on(async {
                SettingsManager::new(app.handle(), &db_for_settings).await
            });
            app.manage(Arc::new(settings_manager));
            app.manage(Arc::new(
                win_v_replacement::WinVReplacementManager::new(),
            ));

            log::info!("Database path: {}", db_path_str);
            if let Ok(log_dir) = app.path().app_log_dir() {
                log::info!("Log directory: {:?}", log_dir);
            }
            let handle = app.handle().clone();
            let db_for_clipboard = db_arc.clone();

            let version = env!("CARGO_PKG_VERSION");
            let title = format!("v{}", version);
            let title_i = MenuItem::with_id(app, "title", &title, false, None::<&str>)?;
            let quit_i = MenuItem::with_id(app, "quit", "Quit Cubby", true, None::<&str>)?;
            let show_i = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
            let separator_i = PredefinedMenuItem::separator(app)?;
            let menu = Menu::with_items(app, &[&title_i, &show_i, &separator_i, &quit_i])?;

            // Pick icon based on current system theme: white for dark, black for light
            let is_dark = dark_light::detect().map(|m| m == dark_light::Mode::Dark).unwrap_or(false);
            let icon_data: &[u8] = if is_dark {
                include_bytes!("../icons/tray_white.png")
            } else {
                include_bytes!("../icons/tray.png")
            };
            let icon = Image::from_bytes(icon_data).map_err(|e| {
                log::info!("Failed to load icon: {:?}", e);
                e
            })?;

            let tray_builder = TrayIconBuilder::with_id("main")
                .icon(icon)
                .menu(&menu);

            let _tray = tray_builder
                .tooltip("Cubby")
                .on_menu_event(move |app, event| {
                    if event.id.as_ref() == "quit" {
                        app.exit(0);
                    } else if event.id.as_ref() == "show" {
                        if let Some(win) = app.get_webview_window("main") {
                            position_window_near_cursor(&win);
                        }
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    if let tauri::tray::TrayIconEvent::Click { button: tauri::tray::MouseButton::Left, .. } = event {
                        if let Some(win) = tray.app_handle().get_webview_window("main") {
                            position_window_near_cursor(&win);
                        }
                    }
                })
                .build(app)?;

            let app_handle = handle.clone();
            let win = app_handle.get_webview_window("main").unwrap();

            {
                let manager = app_handle.state::<Arc<SettingsManager>>();
                let settings = manager.get();
                let mica_effect = settings.mica_effect;
                let theme = settings.theme;
                let round_corners = settings.round_corners;

                // get current system theme
                let current_theme = if theme == "light" {
                    tauri::Theme::Light
                } else if theme == "dark" {
                    tauri::Theme::Dark
                } else {
                    win.theme().unwrap_or_else(|err| {
                        log::error!("THEME:Failed to get system theme: {:?}, defaulting to Light", err);
                        tauri::Theme::Light
                    })
                };

                log::info!("THEME:Applying window effect: {} with theme: {:?} (setting:{:?})", mica_effect, current_theme, theme);

                crate::apply_window_effect(&win, &mica_effect, &current_theme, round_corners);
            }

            let manager = app_handle.state::<Arc<SettingsManager>>();
            let mut shortcut_settings = manager.get();
            let mut shortcuts_ready = match shortcuts::register_shortcuts(
                &app_handle,
                &shortcut_settings.hotkey,
                shortcut_settings.replace_win_v,
            ) {
                Ok(()) => true,
                Err(error) => {
                    log::error!("SHORTCUT: Startup registration failed: {}", error);
                    let replacement_disabled = shortcut_settings.replace_win_v
                        && shortcuts::register_shortcuts(
                            &app_handle,
                            &shortcut_settings.hotkey,
                            false,
                        )
                        .is_ok();

                    let recovered = if replacement_disabled {
                        shortcut_settings.replace_win_v = false;
                        log::warn!("SHORTCUT: Disabled Win+V replacement after startup conflict");
                        true
                    } else {
                        let fallback = "Win+Ctrl+Alt+V";
                        if shortcut_settings.hotkey != fallback
                            && shortcuts::register_shortcuts(&app_handle, fallback, false).is_ok()
                        {
                            shortcut_settings.hotkey = fallback.to_string();
                            shortcut_settings.replace_win_v = false;
                            log::warn!("SHORTCUT: Fell back to {}", fallback);
                            true
                        } else {
                            shortcut_settings.replace_win_v = false;
                            log::error!("SHORTCUT: No startup shortcut could be registered");
                            false
                        }
                    };

                    if let Err(save_error) = manager.save(shortcut_settings.clone()) {
                        log::error!(
                            "SHORTCUT: Failed to persist recovered shortcut settings: {}",
                            save_error
                        );
                    }
                    recovered
                }
            };

            let replacement =
                app_handle.state::<Arc<win_v_replacement::WinVReplacementManager>>();
            if !shortcuts_ready {
                shortcut_settings.replace_win_v = false;
            }
            if let Err(error) =
                replacement.configure(shortcuts_ready && shortcut_settings.replace_win_v)
            {
                log::error!("WIN_V: Startup failed: {}", error);
                shortcut_settings.replace_win_v = false;
                shortcuts_ready = shortcuts::register_shortcuts(
                    &app_handle,
                    &shortcut_settings.hotkey,
                    false,
                )
                .is_ok();
                if let Err(save_error) = manager.save(shortcut_settings.clone()) {
                    log::error!("WIN_V: Failed to persist disabled state: {}", save_error);
                }
            }
            if !shortcuts_ready {
                log::error!("SHORTCUT: Cubby started without a working global shortcut");
            }
            let handle_for_clip = app_handle.clone();
            let db_for_clip = db_for_clipboard.clone();
            clipboard::init(&handle_for_clip, db_for_clip);

            // Start background image migration
            let db_for_migration = db_for_clipboard.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = commands::migrate_images_to_files(&db_for_migration.pool).await {
                    log::error!("Background image migration failed: {}", e);
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::ping,
            commands::get_clips,
            commands::get_clip,
            commands::get_clip_detail,
            commands::paste_clip,
            commands::delete_clip,
            commands::move_to_folder,
            commands::create_folder,
            commands::rename_folder,
            commands::delete_folder,
            commands::search_clips,
            commands::get_folders,
            // Replaced by settings_commands
            settings_commands::get_settings,
            settings_commands::save_settings,
            commands::hide_window,
            commands::get_clipboard_history_size,
            commands::clear_clipboard_history,
            commands::clear_all_clips,
            commands::remove_duplicate_clips,
            commands::register_global_shortcut,
            commands::show_window,
            settings_commands::add_ignored_app,
            settings_commands::remove_ignored_app,
            settings_commands::get_ignored_apps,
            commands::pick_file,
            commands::get_layout_config,
            commands::test_log,
            commands::focus_window,
            commands::refresh_window
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

pub fn position_window_near_cursor(window: &tauri::WebviewWindow) {
    animate_window_show(window);
}

pub fn animate_window_show(window: &tauri::WebviewWindow) {
    if IS_ANIMATING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }

    LAST_SHOW_TIME.store(chrono::Local::now().timestamp_millis(), Ordering::SeqCst);

    let window = window.clone();
    let float_above_taskbar = {
        let manager = window.state::<Arc<crate::settings_manager::SettingsManager>>();
        manager.get().float_above_taskbar
    };

    std::thread::spawn(move || {
        if let Some(monitor) = get_monitor_at_cursor(&window) {
            let scale_factor = monitor.scale_factor();
            let work_area = monitor.work_area();
            let window_width_px = (constants::WINDOW_WIDTH * scale_factor) as u32;
            let window_height_px = (constants::WINDOW_HEIGHT * scale_factor) as u32;
            let margin_px = (constants::WINDOW_MARGIN * scale_factor) as i32;
            let cursor_offset_px = (constants::CURSOR_OFFSET * scale_factor) as i32;
            let cursor = cursor_position().unwrap_or(windows::Win32::Foundation::POINT {
                x: work_area.position.x + work_area.size.width as i32 / 2,
                y: work_area.position.y + work_area.size.height as i32 / 2,
            });

            let work_left = work_area.position.x + margin_px;
            let work_top = work_area.position.y + margin_px;
            let work_right = work_area.position.x + work_area.size.width as i32 - margin_px;
            let work_bottom = work_area.position.y + work_area.size.height as i32 - margin_px;
            let max_x = (work_right - window_width_px as i32).max(work_left);
            let max_y = (work_bottom - window_height_px as i32).max(work_top);

            let right_candidate = cursor.x + cursor_offset_px;
            let left_candidate = cursor.x - cursor_offset_px - window_width_px as i32;
            let mut target_x = if right_candidate + window_width_px as i32 <= work_right {
                right_candidate
            } else {
                left_candidate
            };

            let below_candidate = cursor.y + cursor_offset_px;
            let above_candidate = cursor.y - cursor_offset_px - window_height_px as i32;
            let mut target_y = if below_candidate + window_height_px as i32 <= work_bottom {
                below_candidate
            } else {
                above_candidate
            };

            target_x = target_x.clamp(work_left, max_x);
            target_y = target_y.clamp(work_top, max_y);

            let _ = window.set_size(tauri::Size::Physical(tauri::PhysicalSize {
                width: window_width_px,
                height: window_height_px,
            }));
            let _ = window.set_position(tauri::Position::Physical(tauri::PhysicalPosition {
                x: target_x,
                y: target_y,
            }));
            let _ = window.show();
            let _ = window.set_focus();

            if let Ok(handle) = window.hwnd() {
                use windows::Win32::Foundation::HWND;
                use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_BORDER_COLOR};
                use windows::Win32::UI::WindowsAndMessaging::{
                    SetWindowPos, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
                };
                let hwnd = HWND(handle.0 as _);
                let border_color: u32 = 0xFFFF_FFFE;
                unsafe {
                    let _ = DwmSetWindowAttribute(
                        hwnd,
                        DWMWA_BORDER_COLOR,
                        &border_color as *const _ as *const std::ffi::c_void,
                        std::mem::size_of::<u32>() as u32,
                    );
                }

                if float_above_taskbar {
                    let hwnd_topmost = HWND(-1 as _); // HWND_TOPMOST
                    unsafe {
                        let _ = SetWindowPos(
                            hwnd,
                            Some(hwnd_topmost),
                            0,
                            0,
                            0,
                            0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                        );
                    }
                }
            }
        }
        IS_ANIMATING.store(false, Ordering::SeqCst);
    });
}

pub fn animate_window_hide(
    window: &tauri::WebviewWindow,
    on_done: Option<Box<dyn FnOnce() + Send>>,
) {
    if IS_ANIMATING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }

    let window = window.clone();

    std::thread::spawn(move || {
        let _ = window.hide();
        IS_ANIMATING.store(false, Ordering::SeqCst);

        if let Some(callback) = on_done {
            callback();
        }
    });
}

fn get_data_dir() -> std::path::PathBuf {
    let current_dir = std::env::current_dir().unwrap_or(std::path::PathBuf::from("."));
    match dirs::data_dir() {
        Some(path) => path.join("Cubby Clipboard"),
        None => current_dir.join("Cubby Clipboard"),
    }
}

fn cursor_position() -> Option<windows::Win32::Foundation::POINT> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut point = POINT { x: 0, y: 0 };
    unsafe { GetCursorPos(&mut point).is_ok().then_some(point) }
}

pub fn get_monitor_at_cursor(window: &tauri::WebviewWindow) -> Option<tauri::Monitor> {
    let mut found = None;
    if let Some(point) = cursor_position() {
        if let Ok(monitors) = window.available_monitors() {
            for m in monitors {
                let pos = m.position();
                let size = m.size();
                if point.x >= pos.x
                    && point.x < pos.x + size.width as i32
                    && point.y >= pos.y
                    && point.y < pos.y + size.height as i32
                {
                    found = Some(m);
                    break;
                }
            }
        }
    }
    found.or_else(|| window.current_monitor().ok().flatten())
}

pub fn apply_window_effect(
    window: &tauri::WebviewWindow,
    effect: &str,
    theme: &tauri::Theme,
    round_corners: bool,
) {
    log::info!(
        "THEME:apply_window_effect called: effect={}, theme={:?}, round_corners={}",
        effect,
        theme,
        round_corners
    );
    use window_vibrancy::{apply_mica, apply_tabbed, clear_mica};

    match effect {
        "clear" => {
            if let Err(e) = clear_mica(window) {
                log::error!("THEME:Failed to clear mica: {:?}", e);
            }
            log::info!("THEME:Mica effect cleared");
        }
        "mica" | "dark" => {
            if let Err(e) = clear_mica(window) {
                log::error!("THEME:Failed to clear mica: {:?}", e);
            }
            if let Err(e) = apply_mica(window, Some(matches!(theme, tauri::Theme::Dark))) {
                log::error!("THEME:Failed to apply mica: {:?}", e);
            }
            log::info!("THEME:Applied Mica effect (Theme: {})", theme);
        }
        "mica_alt" | "auto" => {
            if let Err(e) = clear_mica(window) {
                log::error!("THEME:Failed to clear mica: {:?}", e);
            }
            if let Err(e) = apply_tabbed(window, Some(matches!(theme, tauri::Theme::Dark))) {
                log::error!("THEME:Failed to apply tabbed: {:?}", e);
            }
            log::info!("THEME:Applied Tabbed effect (Theme: {})", theme);
        }
        _ => {
            if let Err(e) = clear_mica(window) {
                log::error!("THEME:Failed to clear mica: {:?}", e);
            }
            if let Err(e) = apply_tabbed(window, Some(matches!(theme, tauri::Theme::Dark))) {
                log::error!("THEME:Failed to apply tabbed: {:?}", e);
            }
            log::info!("THEME:Applied Tabbed effect (Theme: {})", theme);
        }
    }

    // Apply DWM rounded corners on Windows 11.
    // "clear" always rounds; Mica/Mica-Alt follow the user setting.
    let use_rounded = effect == "clear" || round_corners;
    if let Ok(handle) = window.hwnd() {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::Graphics::Dwm::{
            DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_DONOTROUND, DWMWCP_ROUND,
        };
        let hwnd = HWND(handle.0 as _);
        let corner_pref = if use_rounded {
            DWMWCP_ROUND.0
        } else {
            DWMWCP_DONOTROUND.0
        };
        unsafe {
            let _ = DwmSetWindowAttribute(
                hwnd,
                DWMWA_WINDOW_CORNER_PREFERENCE,
                &corner_pref as *const _ as *const _,
                std::mem::size_of::<u32>() as u32,
            );
        }
    }
}

pub fn update_tray_icon(tray: &TrayIcon, theme: &tauri::Theme) {
    let icon_data: &[u8] = match theme {
        tauri::Theme::Dark => include_bytes!("../icons/tray_white.png"),
        _ => include_bytes!("../icons/tray.png"),
    };
    if let Ok(icon) = Image::from_bytes(icon_data) {
        let _ = tray.set_icon(Some(icon));
    }
}
