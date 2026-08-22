use std::fs;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use tauri::{
    image::Image,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{TrayIcon, TrayIconBuilder},
    Manager,
};
#[cfg(not(feature = "app-store"))]
use tauri_plugin_autostart::MacosLauncher;

static LAST_SHOW_TIME: AtomicI64 = AtomicI64::new(0);
static SHOW_GENERATION: AtomicU64 = AtomicU64::new(0);
static FLYOUT_WORKER_LIVE: AtomicBool = AtomicBool::new(false);

mod backup;
mod backup_import_optional;
mod cf_html;
mod clip_list;
mod clipboard;
mod clipboard_miss;
mod clipboard_policy;
mod commands;
// SBS-408 compatibility-matrix model. Compiled under `test` as well as
// `dev-harness` on purpose: CI runs `cargo test --all-targets` without the
// feature, so a dev-harness-only module would never have its unit tests run on
// a pull request. Release builds compile neither arm, so none of this reaches
// cubby.exe.
#[cfg(any(test, feature = "dev-harness"))]
pub mod compat_matrix_model;
mod constants;
mod crypto;
mod database;
mod ditto_import;
mod image_persist;
mod log_targets;
mod managed_image;
mod models;
mod ocr;
mod ocr_queue;
pub mod paste_engine;
mod path_grant;
// SBS-219 budgets. Compiled for tests (which measure them) and for the
// dev-harness binaries; a release build compiles neither arm.
#[cfg(any(test, feature = "dev-harness"))]
pub mod perf_budget;
mod search_index;
mod secrets;
mod settings_commands;
mod settings_load;
mod settings_manager;
mod shortcuts;
mod startup_recovery_log;
pub mod win_v_activation;
mod win_v_replacement;
mod window_state;

use database::Database;
use models::get_runtime;
use settings_manager::SettingsManager;
use window_state::{
    click_hits_owned_hwnd, is_new_mouse_press, outside_click_watcher_should_exit,
    should_ignore_blur, AnimationGuard, ANIMATION_LOCK_WAIT, IS_ANIMATING,
};

fn to_plugin_log_target(target: log_targets::LogTarget) -> tauri_plugin_log::Target {
    match target {
        log_targets::LogTarget::Stdout => {
            tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout)
        }
        log_targets::LogTarget::Webview => {
            tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Webview)
        }
        // Folder vs LogDir is decided by `persistent_log_sink` so the rule is
        // unit-tested without a Tauri builder. The arm still names both
        // TargetKinds so the SBS-837 release gate can prove nothing swapped
        // LogDir for Webview, and so a portable run cannot silently keep
        // TargetKind::LogDir (SBS-776).
        log_targets::LogTarget::LogDir => tauri_plugin_log::Target::new(
            match log_targets::persistent_log_sink(portable_data_dir()) {
                log_targets::PersistentLogSink::Folder(path) => {
                    tauri_plugin_log::TargetKind::Folder {
                        path,
                        file_name: None,
                    }
                }
                log_targets::PersistentLogSink::OsLogDir => {
                    tauri_plugin_log::TargetKind::LogDir { file_name: None }
                }
            },
        ),
    }
}

pub fn run_app() {
    let data_dir = get_data_dir();
    fs::create_dir_all(&data_dir).ok();
    let db_path = data_dir.join("cubby.db");
    let db_path_str = db_path.to_str().unwrap_or("cubby.db").to_string();

    let rt = get_runtime().expect("Failed to get global tokio runtime");
    let _guard = rt.enter();

    let (db, mut startup_log) = match rt.block_on(async { Database::new(&db_path_str).await }) {
        Ok(pair) => pair,
        // The one storage failure a user can cause without anything being
        // broken, and the one they can undo. Panicking here produced a silent
        // launch failure: this runs before the logger exists, so the message
        // went nowhere and the window never appeared.
        Err(crypto::StorageError::KeyNotForThisUser { key_path, detail }) => {
            report_storage_key_locked(&key_path, &detail);
            std::process::exit(1);
        }
        Err(error) => panic!("Cubby storage initialization failed: {error}"),
    };

    // Anything logged in this function is thrown away.
    //
    // `tauri_plugin_log` is not installed until the builder runs, far below, so
    // a `log::info!` or `log::error!` up here goes nowhere -- which silently
    // cost the record of a migration that modified a real user's history, and
    // of a quarantine/restore that rewrote it (SBS-929). `Database::new`
    // returns those lines; this buffer is that same vec, then migrations
    // append. Flushed in `setup` once the logger exists. Add to this rather
    // than logging directly.
    rt.block_on(async {
        startup_log.extend(db.migrate().await.expect("Cubby database migration failed"));
        let migrated = commands::migrate_encrypted_storage(&db)
            .await
            .unwrap_or_else(|error| panic!("Cubby encrypted storage migration failed: {error}"));
        if migrated > 0 {
            startup_log.push((
                log::Level::Info,
                format!("STORAGE: Encrypted {migrated} existing clipboard items"),
            ));
        }
        commands::migrate_clip_format_model(&db)
            .await
            .unwrap_or_else(|error| panic!("Cubby clipboard-format migration failed: {error}"));

        // Last, because the two migrations above rewrite content_hash and can
        // create the duplicates this collapses. Reported rather than fatal: a
        // failure leaves the database exactly as it was on the previous
        // version, which still works, and refusing to start over a hardening
        // step would be a worse outcome than running unconstrained.
        //
        // Both outcomes are recorded. Removing duplicates edits history the
        // user cannot get back, so a run that changed something has to say so;
        // the first release of this migration merged five clips on a real
        // machine and left nothing in the log to show for it.
        match db.enforce_content_hash_uniqueness().await {
            Ok(stats) if stats.removed == 0 => {}
            Ok(stats) => startup_log.push((log::Level::Info, stats.startup_message())),
            Err(error) => startup_log.push((
                log::Level::Error,
                format!("STORAGE: Could not enforce unique clip hashes: {error}"),
            )),
        }
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

    log_builder = log_builder.targets(
        log_targets::log_targets(cfg!(debug_assertions))
            .iter()
            .copied()
            .map(to_plugin_log_target),
    );

    #[allow(unused_mut)]
    let mut builder = tauri::Builder::default();

    #[cfg(not(feature = "app-store"))]
    {
        builder = builder.plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            None,
        ));
        // Not registering the plugin is the enforcement, not just the hiding of
        // a button: `plugin-updater`'s `check` is invoked straight from the
        // frontend, so hiding the UI alone would leave the installed-channel
        // flow one direct call away in a build that must never run it.
        if self_update_supported() {
            builder = builder.plugin(tauri_plugin_updater::Builder::new().build());
        }
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
                    if should_ignore_blur(now, last_show) {
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
                tauri::WindowEvent::Destroyed => {
                    // SBS-1015: an abandoned file-dialog pick must not stay
                    // writable after Settings is gone.
                    if let Err(error) =
                        crate::path_grant::drop_grants_if_settings_window(window.label())
                    {
                        log::warn!(
                            "Could not drop path grants when {} closed: {error}",
                            window.label()
                        );
                    }
                }
                _ => {}
            }
        })
        .setup(move |app| {
            // Flushed before anything else is logged: these describe work that
            // finished before the logger existed, so they belong above the
            // first line of this run rather than interleaved after it. Their
            // timestamps are necessarily the flush time, not the event time.
            for (level, message) in startup_log.drain(..) {
                log::log!(level, "{message}");
            }

            log::info!("Cubby starting...");

            // Initialize Settings Manager
            let db_for_settings = db_arc.clone();
            let settings_manager = get_runtime().unwrap().block_on(async {
                SettingsManager::new(app.handle(), &db_for_settings).await
            });
            app.manage(Arc::new(settings_manager));
            let shortcut_manager = Arc::new(win_v_replacement::WinVReplacementManager::new(
                app.handle().clone(),
            ));
            app.manage(shortcut_manager);

            let handle = app.handle().clone();
            let db_for_clipboard = db_arc.clone();

            let version = env!("CARGO_PKG_VERSION");
            let title = format!("v{}", version);
            let title_i = MenuItem::with_id(app, "title", &title, false, None::<&str>)?;
            let quit_i = MenuItem::with_id(app, "quit", "Quit Cubby", true, None::<&str>)?;
            let show_i = MenuItem::with_id(app, "show", "Show", true, None::<&str>)?;
            let history_i =
                MenuItem::with_id(app, "history", "Open History", true, None::<&str>)?;
            let separator_i = PredefinedMenuItem::separator(app)?;
            let menu = Menu::with_items(
                app,
                &[&title_i, &show_i, &history_i, &separator_i, &quit_i],
            )?;

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
                    } else if event.id.as_ref() == "history" {
                        if let Err(e) = commands::show_history_window(app) {
                            log::warn!("Failed to open the history window: {}", e);
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
            let user_hotkey = shortcut_settings.hotkey.clone();
            // Startup conflicts recover for this session only and are surfaced
            // as a toast. The user's configured hotkey is never persisted over:
            // a transient conflict at boot (another app briefly holding the
            // key) must not permanently rewrite their choice, and the next
            // launch retries the original settings.
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

                    if replacement_disabled {
                        shortcut_settings.replace_win_v = false;
                        log::warn!(
                            "SHORTCUT: Disabled Win+V replacement for this session after startup conflict"
                        );
                        shortcuts::record_startup_notice(
                            "win_v_disabled",
                            &user_hotkey,
                            Some(&user_hotkey),
                        );
                        true
                    } else {
                        let fallback = "Win+Ctrl+Alt+V";
                        if shortcut_settings.hotkey != fallback
                            && shortcuts::register_shortcuts(&app_handle, fallback, false).is_ok()
                        {
                            shortcut_settings.hotkey = fallback.to_string();
                            shortcut_settings.replace_win_v = false;
                            log::warn!("SHORTCUT: Fell back to {} for this session", fallback);
                            shortcuts::record_startup_notice(
                                "fallback_hotkey",
                                &user_hotkey,
                                Some(fallback),
                            );
                            true
                        } else {
                            shortcut_settings.replace_win_v = false;
                            false
                        }
                    }
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
                if shortcuts_ready {
                    shortcuts::record_startup_notice(
                        "win_v_disabled",
                        &user_hotkey,
                        Some(&shortcut_settings.hotkey),
                    );
                }
            }
            if !shortcuts_ready {
                log::error!("SHORTCUT: Cubby started without a working global shortcut");
                shortcuts::record_startup_notice("failed", &user_hotkey, None);
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

            // Refresh cubby.db.bak while this session stays up so a machine
            // that never quits still gets a daily recovery copy (SBS-771).
            // Startup already took one pass in Database::new; this only
            // copies when the existing backup is older than 24h.
            database::start_rolling_backup_scheduler(db_path);

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
            commands::set_clip_ocr_text,
            commands::update_clip_text,
            commands::set_clip_notes,
            commands::toggle_clip_hidden,
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
            commands::reclaim_storage,
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
            commands::pick_backup_save_path,
            commands::pick_backup_file,
            commands::export_backup,
            commands::import_backup,
            commands::get_paste_context,
            commands::get_system_accent_color,
            commands::focus_window,
            commands::refresh_window,
            commands::open_history_window,
            commands::get_clip_details,
            commands::copy_selected_text,
            commands::open_image_window,
            commands::get_source_apps,
            commands::delete_clips,
            commands::set_clips_pinned,
            commands::move_clips_to_folder,
            ocr_queue::get_ocr_queue_status,
            ocr_queue::set_ocr_queue_paused,
            ocr_queue::retry_failed_ocr,
            ocr_queue::rescan_clip_ocr,
            clipboard::get_clipboard_capture_status,
            shortcuts::get_hotkey_startup_notice
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// How the flyout anchors when it opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FlyoutRequest {
    Show(ShowAnchor),
    Hide,
}

fn pending_flyout() -> &'static Mutex<Option<(tauri::WebviewWindow, FlyoutRequest)>> {
    static PENDING: OnceLock<Mutex<Option<(tauri::WebviewWindow, FlyoutRequest)>>> =
        OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(None))
}

/// Invert a queued toggle so two presses cancel, instead of queueing hide+hide.
fn next_toggle_request(pending: Option<FlyoutRequest>, visible_and_focused: bool) -> FlyoutRequest {
    match pending {
        Some(FlyoutRequest::Hide) => FlyoutRequest::Show(ShowAnchor::Cursor),
        Some(FlyoutRequest::Show(_)) => FlyoutRequest::Hide,
        None if visible_and_focused => FlyoutRequest::Hide,
        None => FlyoutRequest::Show(ShowAnchor::Cursor),
    }
}

/// Wait for the show/hide lock on a worker. Never sleep on the caller: tray
/// clicks and hotkeys run on Tauri's event loop, and the show worker needs
/// that loop to answer monitor/hwnd queries.
#[cfg(test)]
fn with_animation_guard_nonblocking(
    on_ready: impl FnOnce(Option<AnimationGuard>) + Send + 'static,
) {
    std::thread::spawn(move || {
        let guard = AnimationGuard::acquire()
            .or_else(|| AnimationGuard::acquire_within(ANIMATION_LOCK_WAIT));
        on_ready(guard);
    });
}

fn request_flyout(window: tauri::WebviewWindow, request: FlyoutRequest) {
    *pending_flyout().lock().unwrap() = Some((window, request));
    kick_flyout_worker();
}

/// Hotkey toggle: invert a still-queued request so a double-tap is a no-op,
/// not hide+hide or show+show against the same visible state.
pub fn request_flyout_toggle(window: &tauri::WebviewWindow, visible_and_focused: bool) {
    let mut slot = pending_flyout().lock().unwrap();
    let next = next_toggle_request(
        slot.as_ref().map(|(_, request)| *request),
        visible_and_focused,
    );
    *slot = Some((window.clone(), next));
    drop(slot);
    kick_flyout_worker();
}

fn kick_flyout_worker() {
    if FLYOUT_WORKER_LIVE
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    std::thread::spawn(|| {
        // Clear the latch on panic as well as on a normal drain, then start a
        // replacement if a request arrived during unwind (SBS-990).
        struct FlyoutWorkerGuard;
        impl Drop for FlyoutWorkerGuard {
            fn drop(&mut self) {
                FLYOUT_WORKER_LIVE.store(false, Ordering::SeqCst);
                if pending_flyout()
                    .lock()
                    .map(|slot| slot.is_some())
                    .unwrap_or(false)
                {
                    kick_flyout_worker();
                }
            }
        }
        let _guard = FlyoutWorkerGuard;
        loop {
            let job = pending_flyout().lock().unwrap().take();
            let Some((window, request)) = job else {
                return;
            };
            let Some(guard) = AnimationGuard::acquire()
                .or_else(|| AnimationGuard::acquire_within(ANIMATION_LOCK_WAIT))
            else {
                log::warn!("WINDOW: Flyout request dropped, animation lock still held after 1s");
                continue;
            };
            match request {
                FlyoutRequest::Show(anchor) => run_window_show(window, anchor, guard),
                FlyoutRequest::Hide => {
                    let _ = window.hide();
                    drop(guard);
                }
            }
        }
    });
}

pub fn animate_window_show(window: &tauri::WebviewWindow, anchor: ShowAnchor) {
    request_flyout(window.clone(), FlyoutRequest::Show(anchor));
}

/// Hide once the show/hide lock is free. Drops the request if the lock is
/// still held after the wait — unlike a paste callback, a toggle must not
/// hide without mutual exclusion.
pub fn animate_window_hide_when_idle(window: &tauri::WebviewWindow) {
    request_flyout(window.clone(), FlyoutRequest::Hide);
}

fn run_window_show(
    window: tauri::WebviewWindow,
    anchor: ShowAnchor,
    animation_guard: AnimationGuard,
) {
    LAST_SHOW_TIME.store(chrono::Local::now().timestamp_millis(), Ordering::SeqCst);
    let show_generation = SHOW_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;

    let float_above_taskbar = {
        let manager = window.state::<Arc<crate::settings_manager::SettingsManager>>();
        manager.get().float_above_taskbar
    };

    remember_foreground_window(&window);

    let _animation_guard = animation_guard;
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
                SetWindowPos, HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
            };
            let hwnd = HWND(handle.0 as _);
            // Hide does not clear WS_EX_TOPMOST. Always set the requested
            // z-order so turning the setting off actually unsticks the flyout.
            let insert_after = if float_above_taskbar {
                HWND_TOPMOST
            } else {
                HWND_NOTOPMOST
            };
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    Some(insert_after),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
        }

        if !asset_capture_enabled() {
            watch_for_outside_click(window.clone(), show_generation);
        }
    }
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
    let Some(animation_guard) = AnimationGuard::acquire() else {
        // Another show/hide is in flight. A cosmetic hide (no callback) can be
        // skipped like before — the blur handler and outside-click watcher will
        // fire again. But a callback carries a paste's focus-restore + Ctrl+V
        // (or refresh_window's re-show): dropping it silently used to leave the
        // clipboard set with nothing pasted and the flyout stuck open. Wait
        // briefly for the lock, then hide and run the callback regardless — a
        // missed animation frame is cosmetic, a missed paste is not.
        let Some(callback) = on_done else {
            return;
        };
        let window = window.clone();
        std::thread::spawn(move || {
            let animation_guard = AnimationGuard::acquire_within(ANIMATION_LOCK_WAIT);
            if animation_guard.is_none() {
                log::warn!(
                    "WINDOW: Hide callback proceeding without animation lock (still held after 1s)"
                );
            }
            let _ = window.hide();
            drop(animation_guard);
            callback();
        });
        return;
    };

    let window = window.clone();

    std::thread::spawn(move || {
        let animation_guard = animation_guard;
        let _ = window.hide();
        drop(animation_guard);

        if let Some(callback) = on_done {
            callback();
        }
    });
}

/// Whether this build may run the installed-channel self-update.
///
/// Store builds compile the updater out entirely. Portable builds are a runtime
/// decision, so this is the single source both the plugin registration and the
/// `self_update_available` capability read: the updater installs an NSIS
/// package, which would put an installed Cubby beside the portable one and
/// split the app from the data folder the user carries with it.
pub(crate) fn self_update_supported() -> bool {
    self_update_supported_for(portable_data_dir().is_some())
}

/// The rule itself, split out so both arms are testable. `portable_data_dir`
/// reads the running executable's folder, which a test process cannot stand in
/// for without shipping a `portable.txt` beside the test binary.
fn self_update_supported_for(portable: bool) -> bool {
    !cfg!(feature = "app-store") && !portable
}

/// Thin wrapper over `log_targets::portable_log_dir` for
/// `commands::excluded_log_dir`. The plugin mapper calls
/// `persistent_log_sink` itself; both share one match.
pub(crate) fn portable_log_dir(
    portable_root: Option<std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
    log_targets::portable_log_dir(portable_root)
}

/// Explain an unreadable storage key before exiting.
///
/// This runs before `tauri_plugin_log` and before any window exists, so a
/// native message box is the only way to say anything at all. Deliberately does
/// not delete or rewrite the key or the database: they are intact and the
/// original Windows account can still read them, so every recovery below is
/// non-destructive and the user's to choose.
fn report_storage_key_locked(key_path: &std::path::Path, detail: &str) {
    let portable = portable_data_dir().is_some();
    let location = key_path.parent().unwrap_or(key_path).display().to_string();
    let body = format!(
        "Cubby's clipboard history here was encrypted by a different Windows account, so this \
         account cannot open it.\n\n\
         Windows ties the encryption key to the account that created it. {context}\n\n\
         Nothing has been deleted or changed. Your history is still readable from the original \
         account.\n\n\
         To continue, choose one:\n\n\
         1. Sign in as the original Windows account and run Cubby there.\n\
         2. From that account, use Settings, Back up history, then import the backup here.\n\
         3. Start a separate, empty history for this account by moving or renaming this folder:\n\
         \x20  {location}\n\
         \x20  Cubby creates a fresh one on the next launch. Keep the old folder if you ever want \
         to go back to it.\n\n\
         Technical detail: {detail}",
        context = if portable {
            "This portable folder was carried to another Windows user or PC."
        } else {
            "This usually means the data folder was copied from another user profile or machine."
        },
    );
    show_startup_error("Cubby cannot open this clipboard history", &body);
    eprintln!("STORAGE: {body}");
}

#[cfg(target_os = "windows")]
fn show_startup_error(title: &str, body: &str) {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

    let title = HSTRING::from(title);
    let body = HSTRING::from(body);
    unsafe {
        MessageBoxW(None, &body, &title, MB_OK | MB_ICONERROR);
    }
}

#[cfg(not(target_os = "windows"))]
fn show_startup_error(_title: &str, _body: &str) {}

/// Portable data directory, or None for a normal installed run.
///
/// Cubby runs in portable mode when a `portable.txt` marker sits next to the
/// executable (the portable download ships one). In that mode every piece of
/// state (database, images, `storage.key`, settings, and logs) lives in
/// `<exe_dir>/data`, so nothing is written to AppData or the registry.
///
/// History stays encrypted with the Windows account key, so a portable copy is
/// fully portable on the same PC and account. Carried to a different account it
/// does *not* start fresh: DPAPI refuses the key, and startup explains the
/// recovery options rather than deleting anything. See
/// `report_storage_key_locked`.
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

#[cfg(test)]
fn point_is_inside_rect(
    point: windows::Win32::Foundation::POINT,
    rect: windows::Win32::Foundation::RECT,
) -> bool {
    point.x >= rect.left && point.x < rect.right && point.y >= rect.top && point.y < rect.bottom
}

fn any_mouse_button_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, VK_LBUTTON, VK_MBUTTON, VK_RBUTTON,
    };
    unsafe {
        GetAsyncKeyState(VK_LBUTTON.0 as i32) < 0
            || GetAsyncKeyState(VK_RBUTTON.0 as i32) < 0
            || GetAsyncKeyState(VK_MBUTTON.0 as i32) < 0
    }
}

/// Labels whose windows count as "inside Cubby" for the outside-click hide.
/// Blur already keeps the flyout up while Settings exists; History and the
/// image viewer are the same class of owned window.
const OWNED_WINDOW_LABELS: &[&str] = &[
    "main",
    "settings",
    commands::HISTORY_WINDOW_LABEL,
    commands::IMAGE_WINDOW_LABEL,
];

fn hwnd_raw(hwnd: windows::Win32::Foundation::HWND) -> isize {
    hwnd.0 as isize
}

fn top_window_at(
    point: windows::Win32::Foundation::POINT,
) -> Option<windows::Win32::Foundation::HWND> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetAncestor, IsWindowVisible, WindowFromPoint, GA_ROOT,
    };
    let hwnd = unsafe { WindowFromPoint(point) };
    if hwnd.0.is_null() {
        return None;
    }
    let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
    let top = if root.0.is_null() { hwnd } else { root };
    if !unsafe { IsWindowVisible(top).as_bool() } {
        return None;
    }
    Some(top)
}

fn visible_owned_hwnds(app: &tauri::AppHandle) -> Vec<isize> {
    OWNED_WINDOW_LABELS
        .iter()
        .filter_map(|label| {
            let owned = app.get_webview_window(label)?;
            if !owned.is_visible().unwrap_or(false) {
                return None;
            }
            owned.hwnd().ok().map(|handle| handle.0 as isize)
        })
        .collect()
}

fn click_is_on_owned_window(
    app: &tauri::AppHandle,
    cursor: windows::Win32::Foundation::POINT,
) -> bool {
    let Some(top) = top_window_at(cursor) else {
        return false;
    };
    click_hits_owned_hwnd(hwnd_raw(top), true, &visible_owned_hwnds(app))
}

fn watch_for_outside_click(window: tauri::WebviewWindow, generation: u64) {
    std::thread::spawn(move || {
        // Seed from the current buttons so a drag that opened the flyout is
        // not treated as a rising edge on the first poll.
        let mut buttons_were_down = any_mouse_button_down();

        loop {
            if outside_click_watcher_should_exit(
                SHOW_GENERATION.load(Ordering::SeqCst),
                generation,
                window.is_visible().unwrap_or(false),
            ) {
                break;
            }

            let buttons_down = any_mouse_button_down();

            if is_new_mouse_press(buttons_down, buttons_were_down) {
                if let Some(cursor) = cursor_position() {
                    if !click_is_on_owned_window(window.app_handle(), cursor) {
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
mod portable_tests {
    use super::{portable_data_dir, self_update_supported, self_update_supported_for};

    /// The wiring from the real environment into the rule. The test binary has
    /// no `portable.txt` beside it, so this pins the installed path end to end;
    /// the portable arm is covered directly below.
    #[test]
    fn the_running_build_resolves_its_own_update_capability() {
        assert!(portable_data_dir().is_none(), "test binary is not portable");
        assert_eq!(self_update_supported(), self_update_supported_for(false));
    }

    /// The portable arm, which the test binary cannot reach through
    /// `portable_data_dir`. A portable build must never offer the
    /// installed-channel update: it downloads an NSIS installer, which would
    /// install Cubby somewhere else and split the app from the data folder the
    /// user carries with it.
    #[test]
    fn portable_builds_never_self_update() {
        assert!(!self_update_supported_for(true));
    }

    #[test]
    fn installed_builds_self_update_unless_built_for_the_store() {
        assert_eq!(
            self_update_supported_for(false),
            !cfg!(feature = "app-store")
        );
    }
}

#[cfg(test)]
mod flyout_tests {
    use super::{
        calculate_horizontal_placement, calculate_vertical_placement, click_hits_owned_hwnd,
        fit_window_width, next_toggle_request, point_is_inside_rect,
        with_animation_guard_nonblocking, AnimationGuard, FlyoutRequest, ShowAnchor,
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

    #[test]
    fn hides_when_the_top_window_is_not_an_owned_hwnd() {
        let owned = [10_isize, 20];
        assert!(click_hits_owned_hwnd(10, true, &owned));
        assert!(
            !click_hits_owned_hwnd(99, true, &owned),
            "a click on another app must hide even if it sits over an owned rect"
        );
    }

    #[test]
    fn ignores_hidden_owned_windows() {
        assert!(
            !click_hits_owned_hwnd(10, true, &[]),
            "a hidden Settings/History/image window must not block hide"
        );
        assert!(!click_hits_owned_hwnd(10, false, &[10]));
    }

    #[test]
    fn a_second_toggle_inverts_the_queued_action() {
        assert_eq!(
            next_toggle_request(None, true),
            FlyoutRequest::Hide,
            "first press on a visible flyout hides"
        );
        assert_eq!(
            next_toggle_request(Some(FlyoutRequest::Hide), true),
            FlyoutRequest::Show(ShowAnchor::Cursor),
            "second press must not queue another hide"
        );
        assert_eq!(
            next_toggle_request(None, false),
            FlyoutRequest::Show(ShowAnchor::Cursor)
        );
        assert_eq!(
            next_toggle_request(Some(FlyoutRequest::Show(ShowAnchor::Bottom)), false),
            FlyoutRequest::Hide
        );
    }

    #[test]
    fn waiting_for_the_show_lock_does_not_block_the_caller() {
        let _held = AnimationGuard::acquire().expect("lock should be free");
        let started = std::time::Instant::now();
        let (tx, rx) = std::sync::mpsc::channel();
        with_animation_guard_nonblocking(move |guard| {
            let _ = tx.send(guard.is_some());
        });
        assert!(
            started.elapsed() < std::time::Duration::from_millis(50),
            "the caller must return before the 1s lock wait"
        );
        assert_eq!(
            rx.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Empty),
            "the worker must still be waiting on the held lock"
        );
    }
}
