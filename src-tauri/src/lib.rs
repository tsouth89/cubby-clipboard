use std::fs;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
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
static SHOW_GENERATION: AtomicU64 = AtomicU64::new(0);

mod clipboard;
mod commands;
mod constants;
mod crypto;
mod database;
mod ditto_import;
mod models;
mod ocr;
mod ocr_queue;
pub mod paste_engine;
mod search_index;
mod secrets;
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

    let db = rt
        .block_on(async { Database::new(&db_path_str).await })
        .unwrap_or_else(|error| panic!("Cubby storage initialization failed: {error}"));

    rt.block_on(async {
        db.migrate().await.expect("Cubby database migration failed");
        let migrated = commands::migrate_encrypted_storage(&db)
            .await
            .unwrap_or_else(|error| panic!("Cubby encrypted storage migration failed: {error}"));
        if migrated > 0 {
            log::info!("STORAGE: Encrypted {} existing clipboard items", migrated);
        }
        commands::migrate_clip_format_model(&db)
            .await
            .unwrap_or_else(|error| panic!("Cubby clipboard-format migration failed: {error}"));
    });

    let db_arc = Arc::new(db);
    let search_db = db_arc.clone();
    rt.spawn(async move {
        if let Err(error) = search_db
            .search_index
            .ensure_ready(&search_db.pool, &search_db.crypto)
            .await
        {
            log::error!("SEARCH: Could not build the in-memory index: {error}");
        }
    });

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
        .level(if cfg!(debug_assertions) {
            log::LevelFilter::Debug
        } else {
            log::LevelFilter::Info
        })
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
            log::info!("Second instance detected; showing the existing Cubby window");
            if let Some(window) = app.get_webview_window("main") {
                position_window_near_cursor(&window);
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
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
                    if asset_capture_enabled() {
                        return;
                    }
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

                        let win_clone = win.clone();
                        std::thread::spawn(move || {
                            crate::animate_window_hide(&win_clone, None);
                        });
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
            let shortcut_manager =
                win_v_replacement::WinVReplacementManager::new(app.handle().clone())
                    .map_err(std::io::Error::other)?;
            app.manage(Arc::new(shortcut_manager));

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
                            position_window_from_taskbar(&win);
                        }
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    if let tauri::tray::TrayIconEvent::Click { button: tauri::tray::MouseButton::Left, .. } = event {
                        if let Some(win) = tray.app_handle().get_webview_window("main") {
                            position_window_from_taskbar(&win);
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
            if let Err(error) = replacement.configure(
                shortcuts_ready && shortcut_settings.replace_win_v,
                Some(shortcut_settings.hotkey.clone()),
            ) {
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

            // Start background retention maintenance after encrypted storage is ready.
            let db_for_migration = db_for_clipboard.clone();
            let retention_settings = manager.get();
            tauri::async_runtime::spawn(async move {
                match commands::enforce_retention_in_pool(
                    &db_for_migration.pool,
                    retention_settings.max_items,
                    retention_settings.auto_delete_days,
                )
                .await
                {
                    Ok((deleted, image_paths)) => {
                        commands::remove_clip_image_files(&db_for_migration.image_dir, image_paths);
                        if deleted > 0 {
                            // The eager index build can race ahead of retention; drop the
                            // deleted clips' decrypted documents so they don't linger in
                            // memory. The next search rebuilds without them.
                            db_for_migration.search_index.invalidate();
                            log::info!("STARTUP: Retention removed {} expired or overflow items", deleted);
                        }
                    }
                    Err(error) => log::error!("STARTUP: Retention maintenance failed: {}", error),
                }
            });

            // Asset capture sessions open immediately and drive their staged UI from
            // the frontend. Debug builds only; see asset_capture_enabled().
            let asset_capture = asset_capture_enabled();

            // First launch: surface the flyout so the welcome overlay is visible.
            // Otherwise Cubby starts hidden in the tray and a new user has no idea
            // it's running or how to open it.
            if asset_capture || !manager.get().has_completed_onboarding {
                if let Some(win) = app_handle.get_webview_window("main") {
                    crate::position_window_near_cursor(&win);
                }
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_clips,
            commands::paste_clip,
            commands::copy_clip,
            commands::paste_ocr_text,
            commands::copy_ocr_text,
            commands::delete_clip,
            commands::toggle_clip_pin,
            commands::move_to_folder,
            commands::create_folder,
            commands::rename_folder,
            commands::delete_folder,
            commands::search_clips,
            commands::get_folders,
            // Replaced by settings_commands
            settings_commands::get_settings,
            settings_commands::save_settings,
            settings_commands::complete_onboarding,
            commands::get_clipboard_history_size,
            commands::get_storage_usage,
            commands::apply_retention,
            commands::clear_unpinned_clips,
            commands::clear_all_clips,
            commands::remove_duplicate_clips,
            commands::import_from_ditto,
            settings_commands::add_ignored_app,
            settings_commands::remove_ignored_app,
            settings_commands::get_ignored_apps,
            commands::pick_file,
            commands::pick_ditto_database,
            commands::get_paste_context,
            commands::get_system_accent_color,
            commands::focus_window,
            commands::refresh_window,
            ocr_queue::get_ocr_queue_status,
            ocr_queue::set_ocr_queue_paused,
            ocr_queue::retry_failed_ocr,
            clipboard::get_clipboard_capture_status
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// How the flyout anchors when it opens.
#[derive(Clone, Copy)]
pub enum ShowAnchor {
    /// Anchor near the mouse cursor (hotkey): center the flyout horizontally,
    /// prefer its full height below the cursor, then flip it above the cursor.
    Cursor,
    /// Anchor to the bottom of the work area (taskbar/tray click): a full-height
    /// window rising from the taskbar, which is what a tray click expects.
    Bottom,
}

pub fn position_window_near_cursor(window: &tauri::WebviewWindow) {
    animate_window_show(window, ShowAnchor::Cursor);
}

/// Opens the flyout from the taskbar as a full-height window rising from the
/// bottom. Used when the user clicks the tray icon, where the cursor is at the
/// taskbar and a compact list would feel wrong.
pub fn position_window_from_taskbar(window: &tauri::WebviewWindow) {
    animate_window_show(window, ShowAnchor::Bottom);
}

pub fn animate_window_show(window: &tauri::WebviewWindow, anchor: ShowAnchor) {
    if IS_ANIMATING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }

    LAST_SHOW_TIME.store(chrono::Local::now().timestamp_millis(), Ordering::SeqCst);
    let show_generation = SHOW_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;

    let window = window.clone();
    let float_above_taskbar = {
        let manager = window.state::<Arc<crate::settings_manager::SettingsManager>>();
        manager.get().float_above_taskbar
    };

    remember_foreground_window(&window);

    std::thread::spawn(move || {
        if let Some(monitor) = get_monitor_at_cursor(&window) {
            let scale_factor = monitor.scale_factor();
            let work_area = monitor.work_area();
            let window_width_px = (constants::WINDOW_WIDTH * scale_factor) as u32;
            let desired_height_px = (constants::WINDOW_HEIGHT * scale_factor) as u32;
            let minimum_height_px = (constants::MIN_WINDOW_HEIGHT * scale_factor) as u32;
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
            let window_width_px = fit_window_width(window_width_px, work_left, work_right);
            let target_x =
                calculate_horizontal_placement(cursor.x, work_left, work_right, window_width_px);

            let (target_y, window_height_px) = match anchor {
                ShowAnchor::Cursor => calculate_vertical_placement(
                    cursor.y,
                    work_top,
                    work_bottom,
                    desired_height_px,
                    cursor_offset_px,
                ),
                ShowAnchor::Bottom => {
                    // Full-height window anchored to the bottom of the work area.
                    let height = desired_height_px
                        .min((work_bottom - work_top).max(minimum_height_px as i32) as u32);
                    ((work_bottom - height as i32).max(work_top), height)
                }
            };

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

            suppress_native_window_frame(&window);

            if let Ok(handle) = window.hwnd() {
                use windows::Win32::Foundation::HWND;
                use windows::Win32::UI::WindowsAndMessaging::{
                    SetWindowPos, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
                };
                let hwnd = HWND(handle.0 as _);

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

            if !asset_capture_enabled() {
                watch_for_outside_click(window.clone(), show_generation);
            }
        }
        IS_ANIMATING.store(false, Ordering::SeqCst);
    });
}

fn remember_foreground_window(window: &tauri::WebviewWindow) {
    #[cfg(target_os = "windows")]
    {
        let cubby_hwnd = window.hwnd().ok().map(|handle| handle.0 as isize);
        if let Some(foreground) = paste_engine::remember_foreground_window(cubby_hwnd) {
            log::debug!("FOCUS: remembered foreground window {foreground:#x}");
        }
    }
}

pub fn restore_previous_foreground_window() -> bool {
    paste_engine::restore_previous_foreground_window()
}

fn suppress_native_window_frame(window: &tauri::WebviewWindow) {
    let _ = window.set_shadow(false);

    #[cfg(target_os = "windows")]
    if let Ok(handle) = window.hwnd() {
        use windows::Win32::Foundation::HWND;
        use windows::Win32::Graphics::Dwm::{DwmSetWindowAttribute, DWMWA_BORDER_COLOR};

        // DWMWA_COLOR_NONE prevents Windows 11 from drawing its focused accent border.
        let border_color: u32 = 0xFFFF_FFFE;
        unsafe {
            let _ = DwmSetWindowAttribute(
                HWND(handle.0 as _),
                DWMWA_BORDER_COLOR,
                &border_color as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<u32>() as u32,
            );
        }
    }
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

/// Portable data directory, or None for a normal installed run.
///
/// Cubby runs in portable mode when a `portable.txt` marker sits next to the
/// executable (the portable download ships one). In that mode every piece of
/// state (database, images, `storage.key`, settings) lives in `<exe_dir>/data`,
/// so nothing is written to AppData or the registry. History stays encrypted
/// with the machine's Windows account key, so a portable copy is fully portable
/// on the same PC/account; carried to a different account it starts fresh.
pub fn portable_data_dir() -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    if dir.join("portable.txt").exists() {
        Some(dir.join("data"))
    } else {
        None
    }
}

/// True while the local asset-capture tooling is driving the UI, which needs the
/// flyout to stay open instead of auto-hiding on blur.
///
/// Debug builds only. A release build must never let an environment variable
/// disable auto-hide or the outside-click watcher, so this compiles to a
/// constant `false` there and the call sites optimize away. Matches the
/// frontend gate (`VITE_CUBBY_ASSET_CAPTURE === '1'`) exactly: presence alone is
/// not enough, or `VITE_CUBBY_ASSET_CAPTURE=0` would enable the Rust half while
/// the frontend half stayed off.
#[cfg(debug_assertions)]
fn asset_capture_enabled() -> bool {
    std::env::var("VITE_CUBBY_ASSET_CAPTURE").is_ok_and(|value| value == "1")
}

#[cfg(not(debug_assertions))]
fn asset_capture_enabled() -> bool {
    false
}

pub(crate) fn get_data_dir() -> std::path::PathBuf {
    // Optional override for tests and intentional cross-channel debugging.
    #[cfg(debug_assertions)]
    if let Some(path) = std::env::var_os("CUBBY_DATA_DIR") {
        return std::path::PathBuf::from(path);
    }

    if let Some(portable) = portable_data_dir() {
        return portable;
    }

    let current_dir = std::env::current_dir().unwrap_or(std::path::PathBuf::from("."));
    let base = match dirs::data_dir() {
        Some(path) => path.join("Cubby Clipboard"),
        None => current_dir.join("Cubby Clipboard"),
    };

    // Keep `pnpm tauri dev` history out of the installed release database so a
    // mismatched schema or encryption build cannot corrupt daily-driver data
    // (SOU-227). Release builds continue to use the stable path.
    #[cfg(debug_assertions)]
    {
        base.join("dev")
    }
    #[cfg(not(debug_assertions))]
    {
        base
    }
}

fn cursor_position() -> Option<windows::Win32::Foundation::POINT> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut point = POINT { x: 0, y: 0 };
    unsafe { GetCursorPos(&mut point).is_ok().then_some(point) }
}

fn calculate_vertical_placement(
    cursor_y: i32,
    work_top: i32,
    work_bottom: i32,
    desired_height: u32,
    cursor_offset: i32,
) -> (i32, u32) {
    let below_candidate = cursor_y + cursor_offset;
    let available_below = (work_bottom - below_candidate).max(0) as u32;
    let above_candidate = cursor_y - cursor_offset;
    let available_above = (above_candidate - work_top).max(0) as u32;

    if available_below >= desired_height {
        return (below_candidate, desired_height);
    }

    if available_above >= desired_height {
        return (above_candidate - desired_height as i32, desired_height);
    }

    // Full height fits on neither side. Use the roomier side and shorten only
    // as a last resort without drawing outside the work area.
    let (opens_below, available) = if available_below >= available_above {
        (true, available_below)
    } else {
        (false, available_above)
    };
    let height = desired_height.min(available);
    if opens_below {
        (below_candidate, height)
    } else {
        (above_candidate - height as i32, height)
    }
}

fn calculate_horizontal_placement(
    cursor_x: i32,
    work_left: i32,
    work_right: i32,
    window_width: u32,
) -> i32 {
    let max_x = (work_right - window_width as i32).max(work_left);
    (cursor_x - window_width as i32 / 2).clamp(work_left, max_x)
}

fn fit_window_width(requested_width: u32, work_left: i32, work_right: i32) -> u32 {
    requested_width.min((work_right - work_left).max(1) as u32)
}

fn point_is_inside_rect(
    point: windows::Win32::Foundation::POINT,
    rect: windows::Win32::Foundation::RECT,
) -> bool {
    point.x >= rect.left && point.x < rect.right && point.y >= rect.top && point.y < rect.bottom
}

fn watch_for_outside_click(window: tauri::WebviewWindow, generation: u64) {
    std::thread::spawn(move || {
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            GetAsyncKeyState, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON,
        };
        use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;

        let Ok(raw_handle) = window.hwnd() else {
            return;
        };
        let hwnd = windows::Win32::Foundation::HWND(raw_handle.0 as _);
        let mut buttons_were_down = false;

        loop {
            if SHOW_GENERATION.load(Ordering::SeqCst) != generation
                || !window.is_visible().unwrap_or(false)
            {
                break;
            }

            let buttons_down = unsafe {
                GetAsyncKeyState(VK_LBUTTON.0 as i32) < 0
                    || GetAsyncKeyState(VK_RBUTTON.0 as i32) < 0
                    || GetAsyncKeyState(VK_MBUTTON.0 as i32) < 0
            };

            if buttons_down && !buttons_were_down {
                if let Some(cursor) = cursor_position() {
                    let mut rect = windows::Win32::Foundation::RECT::default();
                    let has_rect = unsafe { GetWindowRect(hwnd, &mut rect).is_ok() };
                    let is_inside = has_rect && point_is_inside_rect(cursor, rect);

                    if !is_inside {
                        animate_window_hide(&window, None);
                        break;
                    }
                }
            }

            buttons_were_down = buttons_down;
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    });
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
    use window_vibrancy::{apply_acrylic, apply_mica, clear_acrylic, clear_mica, clear_tabbed};

    // Keep WebView2's preferred color scheme and the native DWM material on the
    // same resolved theme. This is especially important for Acrylic, whose
    // Windows 11 transient backdrop otherwise may remain light while Cubby is dark.
    if let Err(error) = window.set_theme(Some(*theme)) {
        log::error!("THEME:Failed to set resolved window theme: {:?}", error);
    }

    match effect {
        "solid" | "clear" => {
            if let Err(e) = clear_acrylic(window) {
                log::error!("THEME:Failed to clear acrylic: {:?}", e);
            }
            if let Err(e) = clear_mica(window) {
                log::error!("THEME:Failed to clear mica: {:?}", e);
            }
            if let Err(e) = clear_tabbed(window) {
                log::error!("THEME:Failed to clear tabbed: {:?}", e);
            }
            log::info!("THEME:Window backdrop cleared for solid mode");
        }
        "mica" | "dark" => {
            if let Err(e) = clear_acrylic(window) {
                log::error!("THEME:Failed to clear acrylic: {:?}", e);
            }
            if let Err(e) = clear_mica(window) {
                log::error!("THEME:Failed to clear mica: {:?}", e);
            }
            if let Err(e) = clear_tabbed(window) {
                log::error!("THEME:Failed to clear tabbed: {:?}", e);
            }
            if let Err(e) = apply_mica(window, Some(matches!(theme, tauri::Theme::Dark))) {
                log::error!("THEME:Failed to apply mica: {:?}", e);
            }
            log::info!("THEME:Applied Mica effect (Theme: {})", theme);
        }
        "acrylic" | "mica_alt" | "auto" => {
            if let Err(e) = clear_acrylic(window) {
                log::error!("THEME:Failed to clear acrylic: {:?}", e);
            }
            if let Err(e) = clear_mica(window) {
                log::error!("THEME:Failed to clear mica: {:?}", e);
            }
            if let Err(e) = clear_tabbed(window) {
                log::error!("THEME:Failed to clear tabbed: {:?}", e);
            }
            let tint = if matches!(theme, tauri::Theme::Dark) {
                (18, 18, 20, 115)
            } else {
                (245, 245, 247, 115)
            };
            // clear_mica resets this attribute to light mode. Acrylic does not set it
            // itself on Windows 11, so restore the active app theme before applying it.
            if let Ok(handle) = window.hwnd() {
                use windows::Win32::Foundation::HWND;
                use windows::Win32::Graphics::Dwm::{
                    DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE,
                };
                let hwnd = HWND(handle.0 as _);
                let dark_mode = u32::from(matches!(theme, tauri::Theme::Dark));
                unsafe {
                    if let Err(error) = DwmSetWindowAttribute(
                        hwnd,
                        DWMWA_USE_IMMERSIVE_DARK_MODE,
                        &dark_mode as *const _ as _,
                        std::mem::size_of_val(&dark_mode) as u32,
                    ) {
                        log::error!(
                            "THEME:Failed to set Acrylic immersive dark mode: {:?}",
                            error
                        );
                    }
                }
            }
            if let Err(e) = apply_acrylic(window, Some(tint)) {
                log::error!("THEME:Failed to apply acrylic: {:?}", e);
            }
            // Some Windows 11 builds reset the immersive flag while changing the
            // system backdrop type, so assert it again after Acrylic is active.
            if let Ok(handle) = window.hwnd() {
                use windows::Win32::Foundation::HWND;
                use windows::Win32::Graphics::Dwm::{
                    DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE,
                };
                let hwnd = HWND(handle.0 as _);
                let dark_mode = u32::from(matches!(theme, tauri::Theme::Dark));
                unsafe {
                    if let Err(error) = DwmSetWindowAttribute(
                        hwnd,
                        DWMWA_USE_IMMERSIVE_DARK_MODE,
                        &dark_mode as *const _ as _,
                        std::mem::size_of_val(&dark_mode) as u32,
                    ) {
                        log::error!(
                            "THEME:Failed to restore Acrylic immersive dark mode: {:?}",
                            error
                        );
                    }
                }
            }
            log::info!("THEME:Applied Acrylic effect (Theme: {})", theme);
        }
        _ => {
            if let Err(e) = clear_acrylic(window) {
                log::error!("THEME:Failed to clear acrylic: {:?}", e);
            }
            if let Err(e) = clear_mica(window) {
                log::error!("THEME:Failed to clear mica: {:?}", e);
            }
            if let Err(e) = clear_tabbed(window) {
                log::error!("THEME:Failed to clear tabbed: {:?}", e);
            }
            log::info!("THEME:Unknown window effect; using solid mode");
        }
    }

    // Keep the native window shape aligned with the frontend frame.
    let use_rounded = round_corners;
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

    suppress_native_window_frame(window);
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

#[cfg(test)]
mod flyout_tests {
    use super::{
        calculate_horizontal_placement, calculate_vertical_placement, fit_window_width,
        point_is_inside_rect,
    };
    use windows::Win32::Foundation::{POINT, RECT};

    #[test]
    fn opens_full_height_below_the_cursor_when_space_allows() {
        assert_eq!(
            calculate_vertical_placement(250, 12, 1392, 620, 14),
            (264, 620)
        );
    }

    #[test]
    fn flips_full_height_above_instead_of_shrinking_below() {
        assert_eq!(
            calculate_vertical_placement(962, 12, 1392, 620, 14),
            (328, 620)
        );
    }

    #[test]
    fn opens_full_height_above_near_the_bottom_edge() {
        assert_eq!(
            calculate_vertical_placement(1272, 12, 1392, 620, 14),
            (638, 620)
        );
    }

    #[test]
    fn shortens_on_the_roomier_side_only_when_full_height_fits_neither_side() {
        assert_eq!(
            calculate_vertical_placement(500, 12, 900, 620, 14),
            (12, 474)
        );
        assert_eq!(
            calculate_vertical_placement(400, 12, 900, 620, 14),
            (414, 486)
        );
    }

    #[test]
    fn centers_the_flyout_horizontally_on_the_cursor() {
        assert_eq!(calculate_horizontal_placement(800, 12, 1588, 520), 540);
    }

    #[test]
    fn clamps_centered_placement_to_monitor_edges() {
        assert_eq!(calculate_horizontal_placement(50, 12, 1588, 520), 12);
        assert_eq!(calculate_horizontal_placement(1550, 12, 1588, 520), 1068);
    }

    #[test]
    fn caps_the_flyout_width_to_unusually_narrow_work_areas() {
        assert_eq!(fit_window_width(520, 12, 412), 400);
        assert_eq!(fit_window_width(520, 12, 1588), 520);
    }

    #[test]
    fn detects_points_inside_the_flyout_rectangle() {
        let rect = RECT {
            left: 100,
            top: 200,
            right: 620,
            bottom: 820,
        };

        assert!(point_is_inside_rect(POINT { x: 100, y: 200 }, rect));
        assert!(point_is_inside_rect(POINT { x: 619, y: 819 }, rect));
    }

    #[test]
    fn treats_edges_and_external_clicks_as_outside() {
        let rect = RECT {
            left: 100,
            top: 200,
            right: 620,
            bottom: 820,
        };

        assert!(!point_is_inside_rect(POINT { x: 99, y: 400 }, rect));
        assert!(!point_is_inside_rect(POINT { x: 620, y: 400 }, rect));
        assert!(!point_is_inside_rect(POINT { x: 300, y: 820 }, rect));
    }
}
