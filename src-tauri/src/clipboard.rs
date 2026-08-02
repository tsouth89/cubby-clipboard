use tauri::{AppHandle, Emitter};
// Import functions directly from the crate root
use crate::database::Database;
#[cfg(target_os = "windows")]
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use clipboard_rs::common::RustImage;
#[cfg(not(target_os = "windows"))]
use clipboard_rs::common::RustImageData;
use clipboard_rs::{Clipboard, ClipboardContent, ClipboardContext};
#[cfg(target_os = "windows")]
use clipboard_win::Monitor;
use once_cell::sync::Lazy;
use serde::Serialize;
use sha2::{Digest, Sha256};
#[cfg(target_os = "windows")]
use std::ffi::OsStr;
#[cfg(target_os = "windows")]
use std::os::windows::ffi::OsStrExt;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use uuid::Uuid;
#[cfg(target_os = "windows")]
use windows::Win32::Foundation::MAX_PATH;
#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, GetDC, GetDIBits,
    GetObjectW, ReleaseDC, SelectObject, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HBITMAP,
};
#[cfg(target_os = "windows")]
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};
#[cfg(target_os = "windows")]
use windows::Win32::System::DataExchange::GetClipboardOwner;
#[cfg(target_os = "windows")]
use windows::Win32::System::ProcessStatus::{GetModuleBaseNameW, GetModuleFileNameExW};
#[cfg(target_os = "windows")]
use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ};
#[cfg(target_os = "windows")]
use windows::Win32::UI::Shell::{
    SHGetFileInfoW, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON, SHGFI_USEFILEATTRIBUTES,
};
#[cfg(target_os = "windows")]
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, DrawIconEx, GetForegroundWindow, GetIconInfo, GetWindowThreadProcessId, DI_NORMAL,
    ICONINFO,
};

// GLOBAL STATE: Store the hash of the clip we just pasted ourselves.
// If the next clipboard change matches this hash, we ignore it (don't update timestamp).
static IGNORE_HASH: Lazy<parking_lot::Mutex<Option<String>>> =
    Lazy::new(|| parking_lot::Mutex::new(None));
static LAST_STABLE_HASH: Lazy<parking_lot::Mutex<Option<String>>> =
    Lazy::new(|| parking_lot::Mutex::new(None));
/// Most recent accepted capture, used to forget extension password copies when
/// the clipboard is auto-cleared shortly afterward (SOU-316).
static LAST_ACCEPTED_CAPTURE: Lazy<parking_lot::Mutex<Option<RecentCapture>>> =
    Lazy::new(|| parking_lot::Mutex::new(None));
/// Hash of the last capture the remote relay re-announced. A viewer that echoes
/// our rewrite back (via its remote endpoint) must not trigger another relay,
/// or Cubby and the viewer would ping-pong writes forever.
static LAST_RELAYED_HASH: Lazy<parking_lot::Mutex<Option<String>>> =
    Lazy::new(|| parking_lot::Mutex::new(None));
pub static CLIPBOARD_SYNC: Lazy<Arc<tokio::sync::Mutex<()>>> =
    Lazy::new(|| Arc::new(tokio::sync::Mutex::new(())));

/// Password-manager extensions typically clear within tens of seconds. Keep this
/// short so a deliberate later clear does not erase an intentional keep.
const CLIPBOARD_CLEAR_FORGET_WINDOW: Duration = Duration::from_secs(90);

#[derive(Clone, Debug)]
struct RecentCapture {
    uuid: String,
    captured_at: Instant,
    /// Whether the captured text was credential-shaped at capture time. An
    /// auto-clear only forgets credential-shaped captures: password managers
    /// clear on a timer regardless of what is on the clipboard by then, so an
    /// unconditional forget could delete an innocent note copied after the
    /// password (the wrong-clip bug).
    credential_like: bool,
}

/// Pure helper for SOU-316 unit tests: only empty clears within the window
/// forget the last clip. A later clear must leave history alone.
fn should_forget_recent_capture(
    captured_at: Instant,
    cleared_at: Instant,
    window: Duration,
) -> bool {
    cleared_at
        .checked_duration_since(captured_at)
        .is_some_and(|elapsed| elapsed <= window)
}

/// Capture-listener health (SOU-218). Values are diagnostics only — never clipboard content.
///
/// Defaults:
/// - restart forever with capped exponential backoff (500ms → 30s)
/// - sequence watchdog every 2s; force restart after 5s of missed advances
/// - no user-facing toast (logs + `get_clipboard_capture_status` only)
const CAPTURE_STATE_STOPPED: u8 = 0;
const CAPTURE_STATE_LISTENING: u8 = 1;
const CAPTURE_STATE_RESTARTING: u8 = 2;
const INITIAL_LISTENER_BACKOFF: Duration = Duration::from_millis(500);
const MAX_LISTENER_BACKOFF: Duration = Duration::from_secs(30);
const WATCHDOG_INTERVAL: Duration = Duration::from_secs(2);
const STALE_LISTENER_AFTER: Duration = Duration::from_secs(5);

static CAPTURE_STATE: AtomicU8 = AtomicU8::new(CAPTURE_STATE_STOPPED);
static LAST_CLIPBOARD_EVENT_UNIX_MS: AtomicU64 = AtomicU64::new(0);
static LAST_HANDLED_SEQUENCE: AtomicU32 = AtomicU32::new(0);
static LISTENER_RESTART_COUNT: AtomicU64 = AtomicU64::new(0);
static LAST_CAPTURE_ERROR: Lazy<parking_lot::Mutex<Option<String>>> =
    Lazy::new(|| parking_lot::Mutex::new(None));
#[cfg(target_os = "windows")]
static LISTENER_SHUTDOWN: Lazy<parking_lot::Mutex<Option<clipboard_win::monitor::Shutdown>>> =
    Lazy::new(|| parking_lot::Mutex::new(None));

#[derive(Debug, Clone, Serialize)]
pub struct ClipboardCaptureStatus {
    pub state: String,
    pub listening: bool,
    pub restart_count: u64,
    pub last_event_unix_ms: u64,
    pub last_handled_sequence: u32,
    pub last_error: Option<String>,
}

pub fn set_ignore_hash(hash: String) {
    let mut lock = IGNORE_HASH.lock();
    *lock = Some(hash);
}

pub(crate) fn clear_ignore_hash_if_matches(hash: &str) {
    let mut lock = IGNORE_HASH.lock();
    if lock.as_deref() == Some(hash) {
        lock.take();
    }
}

/// Forget the consecutive-duplicate marker after history is deleted. Without
/// this, deleting a clip and copying the same content again is silently
/// dropped ("capture is broken" from the user's perspective): the marker still
/// holds the deleted clip's hash and suppresses the re-copy.
pub fn reset_capture_dedup() {
    *LAST_STABLE_HASH.lock() = None;
}

fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn capture_state_name(state: u8) -> &'static str {
    match state {
        CAPTURE_STATE_LISTENING => "listening",
        CAPTURE_STATE_RESTARTING => "restarting",
        _ => "stopped",
    }
}

fn set_capture_state(state: u8) {
    CAPTURE_STATE.store(state, Ordering::SeqCst);
}

fn record_capture_error(message: impl Into<String>) {
    let message = message.into();
    log::error!("CLIPBOARD: {message}");
    *LAST_CAPTURE_ERROR.lock() = Some(message);
}

fn note_clipboard_event(sequence: u32) {
    LAST_HANDLED_SEQUENCE.store(sequence, Ordering::SeqCst);
    LAST_CLIPBOARD_EVENT_UNIX_MS.store(unix_now_ms(), Ordering::SeqCst);
}

fn next_listener_backoff(current: Duration) -> Duration {
    current
        .checked_mul(2)
        .unwrap_or(MAX_LISTENER_BACKOFF)
        .min(MAX_LISTENER_BACKOFF)
}

#[tauri::command]
pub fn get_clipboard_capture_status() -> ClipboardCaptureStatus {
    let state = CAPTURE_STATE.load(Ordering::SeqCst);
    ClipboardCaptureStatus {
        state: capture_state_name(state).to_string(),
        listening: state == CAPTURE_STATE_LISTENING,
        restart_count: LISTENER_RESTART_COUNT.load(Ordering::SeqCst),
        last_event_unix_ms: LAST_CLIPBOARD_EVENT_UNIX_MS.load(Ordering::SeqCst),
        last_handled_sequence: LAST_HANDLED_SEQUENCE.load(Ordering::SeqCst),
        last_error: LAST_CAPTURE_ERROR.lock().clone(),
    }
}

pub fn init(app: &AppHandle, db: Arc<Database>) {
    crate::ocr_queue::init(app.clone(), db.clone());
    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
    let app_for_consumer = app.clone();
    let db_for_consumer = db.clone();

    tauri::async_runtime::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            match event {
                ClipboardListenerEvent::Content(snapshot) => {
                    process_clipboard_snapshot(
                        app_for_consumer.clone(),
                        db_for_consumer.clone(),
                        snapshot,
                    )
                    .await;
                }
                ClipboardListenerEvent::Cleared { sequence } => {
                    process_clipboard_clear(
                        app_for_consumer.clone(),
                        db_for_consumer.clone(),
                        sequence,
                    )
                    .await;
                }
            }
        }
        log::error!("CLIPBOARD: Native snapshot queue closed unexpectedly");
    });

    std::thread::Builder::new()
        .name("cubby-clipboard-listener".to_string())
        .spawn(move || run_native_listener(event_tx))
        .unwrap_or_else(|error| panic!("failed to start native clipboard listener: {error}"));
}

type SourceAppInfo = (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    bool,
);

#[derive(Clone, Copy)]
struct SourceAppIdentity {
    process_id: u32,
    is_explicit_owner: bool,
}

struct ClipboardImageRead {
    png_bytes: Vec<u8>,
    width: u32,
    height: u32,
    raw_hash: String,
    decode_ms: u128,
    source_type: &'static str,
}

enum CapturedContent {
    Text {
        content: Vec<u8>,
        preview: String,
        hash: String,
    },
    Image {
        png_bytes: Vec<u8>,
        width: u32,
        height: u32,
        hash: String,
        decode_ms: u128,
        source_type: &'static str,
    },
}

struct ClipboardSnapshot {
    sequence: u32,
    source_app_identity: Option<SourceAppIdentity>,
    content: CapturedContent,
    formats: Vec<CapturedFormat>,
    materialize_ms: u128,
    /// The source application tagged this copy as sensitive (e.g. a password
    /// manager) so clipboard monitors should skip it. See `clipboard_marked_sensitive`.
    sensitive: bool,
}

enum ClipboardListenerEvent {
    Content(ClipboardSnapshot),
    /// Clipboard became empty (or empty-text) after a sequence advance.
    Cleared {
        sequence: u32,
    },
}

/// Returns true when the current clipboard contents are tagged with the
/// well-known `ExcludeClipboardContentFromMonitorProcessing` format. Password
/// managers and other secret-holding apps set this so clipboard history tools
/// skip the copy. Its mere presence means "do not retain"; reading it does not
/// require opening the clipboard, so this is cheap and contention-free.
#[cfg(target_os = "windows")]
fn clipboard_marked_sensitive() -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::System::DataExchange::{
        IsClipboardFormatAvailable, RegisterClipboardFormatW,
    };

    let name: Vec<u16> = "ExcludeClipboardContentFromMonitorProcessing"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let format = unsafe { RegisterClipboardFormatW(PCWSTR(name.as_ptr())) };
    format != 0 && unsafe { IsClipboardFormatAvailable(format) }.is_ok()
}

pub(crate) struct CapturedFormat {
    pub(crate) name: &'static str,
    pub(crate) content: Vec<u8>,
}

#[cfg(target_os = "windows")]
enum ListenerSessionExit {
    /// Snapshot consumer dropped — app is shutting down; do not restart.
    ConsumerGone,
    /// Watchdog or explicit shutdown asked us to recreate the monitor.
    RestartRequested,
    /// Win32 / monitor failure; recreate after backoff.
    Failed(String),
}

#[cfg(target_os = "windows")]
fn request_listener_restart(reason: &str) {
    log::warn!("CLIPBOARD: Requesting listener restart ({reason})");
    if let Some(shutdown) = LISTENER_SHUTDOWN.lock().take() {
        drop(shutdown);
    }
}

#[cfg(target_os = "windows")]
fn spawn_listener_watchdog() {
    static WATCHDOG_STARTED: AtomicU8 = AtomicU8::new(0);
    if WATCHDOG_STARTED.swap(1, Ordering::SeqCst) != 0 {
        return;
    }

    if let Err(error) = std::thread::Builder::new()
        .name("cubby-clipboard-watchdog".to_string())
        .spawn(|| {
            loop {
                std::thread::sleep(WATCHDOG_INTERVAL);
                if CAPTURE_STATE.load(Ordering::SeqCst) != CAPTURE_STATE_LISTENING {
                    continue;
                }

                let current_sequence = unsafe {
                    windows::Win32::System::DataExchange::GetClipboardSequenceNumber()
                };
                let handled = LAST_HANDLED_SEQUENCE.load(Ordering::SeqCst);
                if current_sequence == handled {
                    continue;
                }

                let last_event_ms = LAST_CLIPBOARD_EVENT_UNIX_MS.load(Ordering::SeqCst);
                let now_ms = unix_now_ms();
                let stale_for_ms = now_ms.saturating_sub(last_event_ms);
                if stale_for_ms < STALE_LISTENER_AFTER.as_millis() as u64 {
                    continue;
                }

                // Sequence advanced without a WM_CLIPBOARDUPDATE reaching us —
                // typical after sleep/resume or Explorer restarts when the listener
                // HWND is still "alive" but no longer receiving updates.
                request_listener_restart(&format!(
                    "watchdog: sequence {current_sequence} ahead of handled {handled} for {stale_for_ms}ms"
                ));
            }
        })
    {
        log::error!("CLIPBOARD: Failed to start listener watchdog: {error}");
    }
}

/// Outcome of trying to ingest the clipboard for one sequence number.
#[cfg(target_os = "windows")]
enum CaptureAttempt {
    /// A snapshot or clear event was queued (or the payload is unsupported);
    /// the sequence was marked handled.
    Handled,
    /// Supported content is present but every clipboard open lost the race.
    /// The sequence stays unhandled so the watchdog retries it.
    Deferred,
}

/// Try to materialize and queue the clipboard content behind `sequence`.
///
/// The sequence is marked handled only after a successful materialize (or a
/// confirmed clear / unsupported payload). A contended read leaves it
/// unhandled: the watchdog then sees it stale after [`STALE_LISTENER_AFTER`]
/// and restarts the listener, whose session start retries this capture instead
/// of silently dropping the copy.
///
/// `Err(())` means the snapshot consumer is gone (process teardown).
#[cfg(target_os = "windows")]
fn capture_clipboard_update(
    sequence: u32,
    event_tx: &tokio::sync::mpsc::UnboundedSender<ClipboardListenerEvent>,
) -> Result<CaptureAttempt, ()> {
    let started = Instant::now();

    // File clipboard payloads are references to external paths, not durable
    // clipboard content. Recording them as history creates entries that can
    // silently stop working after a move, disconnect, or target-app mismatch.
    // Ignore both physical and virtual file payloads before reading any text
    // fallback they may advertise. Screenshot tools are the exception: they
    // intentionally add CF_HDROP beside real image data, which Cubby retains
    // as an image rather than as an unreliable file reference.
    let has_file_payload = clipboard_has_file_payload_format();
    let has_image_payload = clipboard_has_image_format();
    if has_file_payload && !has_image_payload {
        note_clipboard_event(sequence);
        log::debug!(
            "CLIPBOARD: Sequence {} contained a file payload; intentionally ignoring it",
            sequence
        );
        return Ok(CaptureAttempt::Handled);
    }

    let source_app_identity = get_clipboard_owner_identity();
    let sensitive = clipboard_marked_sensitive();

    if let Some((content, formats)) = materialize_clipboard_content() {
        note_clipboard_event(sequence);
        let snapshot = ClipboardSnapshot {
            sequence,
            source_app_identity,
            content,
            formats,
            materialize_ms: started.elapsed().as_millis(),
            sensitive,
        };
        return event_tx
            .send(ClipboardListenerEvent::Content(snapshot))
            .map(|_| CaptureAttempt::Handled)
            .map_err(|_| ());
    }

    if has_file_payload && has_image_payload {
        // The hybrid image was advertised but remained unreadable after all
        // bounded attempts. Do not fall through to its path-like text or keep
        // restarting the listener forever for malformed image data.
        note_clipboard_event(sequence);
        log::warn!(
            "CLIPBOARD: Sequence {} advertised a file-backed image but its image data was unreadable",
            sequence
        );
        return Ok(CaptureAttempt::Handled);
    }

    if clipboard_is_cleared() {
        // Empty clipboard (or empty text with no image/files). This is
        // the password-manager auto-clear signal; never treat a new
        // non-empty copy as a clear.
        note_clipboard_event(sequence);
        return event_tx
            .send(ClipboardListenerEvent::Cleared { sequence })
            .map(|_| CaptureAttempt::Handled)
            .map_err(|_| ());
    }

    if clipboard_has_supported_format() {
        log::warn!(
            "CLIPBOARD: Could not materialize sequence {} (clipboard contended); deferring for watchdog retry",
            sequence
        );
        return Ok(CaptureAttempt::Deferred);
    }

    // Nothing we support (custom/private formats only). Mark handled so the
    // watchdog does not treat an ignored format as a dead listener.
    note_clipboard_event(sequence);
    log::debug!(
        "CLIPBOARD: Sequence {} contained no supported text or image payload",
        sequence
    );
    Ok(CaptureAttempt::Handled)
}

/// True when the clipboard advertises a format `materialize_clipboard_content`
/// can read (text or an image). Used to tell "unsupported payload"
/// (mark handled) apart from "supported but contended" (defer and retry).
#[cfg(target_os = "windows")]
fn clipboard_has_supported_format() -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::System::DataExchange::{
        IsClipboardFormatAvailable, RegisterClipboardFormatW,
    };

    const CF_TEXT: u32 = 1;
    const CF_BITMAP: u32 = 2;
    const CF_DIB: u32 = 8;
    const CF_UNICODETEXT: u32 = 13;
    const CF_DIBV5: u32 = 17;

    if [CF_UNICODETEXT, CF_TEXT, CF_DIB, CF_DIBV5, CF_BITMAP]
        .into_iter()
        .any(|format| unsafe { IsClipboardFormatAvailable(format) }.is_ok())
    {
        return true;
    }

    // Some producers put only a registered "PNG" entry on the clipboard.
    let name: Vec<u16> = "PNG".encode_utf16().chain(std::iter::once(0)).collect();
    let png_format = unsafe { RegisterClipboardFormatW(PCWSTR(name.as_ptr())) };
    png_format != 0 && unsafe { IsClipboardFormatAvailable(png_format) }.is_ok()
}

#[cfg(target_os = "windows")]
fn clipboard_has_file_payload_format() -> bool {
    use std::sync::OnceLock;
    use windows::core::PCWSTR;
    use windows::Win32::System::DataExchange::{
        IsClipboardFormatAvailable, RegisterClipboardFormatW,
    };

    const CF_HDROP: u32 = 15;
    if unsafe { IsClipboardFormatAvailable(CF_HDROP) }.is_ok() {
        return true;
    }

    // OLE virtual files (for example Outlook attachments) do not necessarily
    // expose CF_HDROP. Their descriptors still identify the update as a file
    // payload, which Cubby deliberately does not present as durable history.
    static VIRTUAL_FILE_FORMATS: OnceLock<[u32; 3]> = OnceLock::new();
    VIRTUAL_FILE_FORMATS
        .get_or_init(|| {
            [
                "FileGroupDescriptor",
                "FileGroupDescriptorW",
                "FileContents",
            ]
            .map(|format_name| {
                let name: Vec<u16> = format_name
                    .encode_utf16()
                    .chain(std::iter::once(0))
                    .collect();
                unsafe { RegisterClipboardFormatW(PCWSTR(name.as_ptr())) }
            })
        })
        .iter()
        .copied()
        .any(|format| format != 0 && unsafe { IsClipboardFormatAvailable(format) }.is_ok())
}

#[cfg(not(target_os = "windows"))]
fn clipboard_has_file_payload_format() -> bool {
    false
}

#[cfg(target_os = "windows")]
fn clipboard_has_image_format() -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::System::DataExchange::{
        IsClipboardFormatAvailable, RegisterClipboardFormatW,
    };

    const CF_BITMAP: u32 = 2;
    const CF_DIB: u32 = 8;
    const CF_DIBV5: u32 = 17;

    if [CF_DIBV5, CF_DIB, CF_BITMAP]
        .into_iter()
        .any(|format| unsafe { IsClipboardFormatAvailable(format) }.is_ok())
    {
        return true;
    }

    let name: Vec<u16> = "PNG".encode_utf16().chain(std::iter::once(0)).collect();
    let png_format = unsafe { RegisterClipboardFormatW(PCWSTR(name.as_ptr())) };
    png_format != 0 && unsafe { IsClipboardFormatAvailable(png_format) }.is_ok()
}

#[cfg(not(target_os = "windows"))]
fn clipboard_has_image_format() -> bool {
    false
}

#[cfg(target_os = "windows")]
fn run_listener_session(
    monitor: &mut Monitor,
    event_tx: &tokio::sync::mpsc::UnboundedSender<ClipboardListenerEvent>,
) -> ListenerSessionExit {
    loop {
        match monitor.recv() {
            Ok(true) => {
                let sequence =
                    unsafe { windows::Win32::System::DataExchange::GetClipboardSequenceNumber() };
                if capture_clipboard_update(sequence, event_tx).is_err() {
                    return ListenerSessionExit::ConsumerGone;
                }
            }
            Ok(false) => return ListenerSessionExit::RestartRequested,
            Err(error) => return ListenerSessionExit::Failed(error.to_string()),
        }
    }
}

/// True when the clipboard is empty or only holds empty text (and no image/files).
/// Returns false when the clipboard cannot be opened (contention) so we never
/// treat a lock miss as an auto-clear.
fn clipboard_is_cleared() -> bool {
    const ATTEMPTS: u32 = 5;

    for attempt in 0..ATTEMPTS {
        if let Ok(ctx) = ClipboardContext::new() {
            if let Ok(files) = ctx.get_files() {
                if !files.is_empty() {
                    return false;
                }
            }
            if ctx.get_image().is_ok() {
                return false;
            }
            // Empty plain text must not count as a clear if HTML/RTF still hold
            // content (materialize can miss those when get_text is empty).
            if let Ok(html) = ctx.get_html() {
                if !html.is_empty() {
                    return false;
                }
            }
            if let Ok(rtf) = ctx.get_rich_text() {
                if !rtf.is_empty() {
                    return false;
                }
            }
            match ctx.get_text() {
                Ok(text) if !text.is_empty() => return false,
                // Empty text only counts as a clear when nothing else is on the
                // clipboard. An empty CF_UNICODETEXT written alongside an app's
                // private format is a normal copy of unsupported content, and
                // treating it as a clear used to delete the previous capture.
                Ok(_) => return clipboard_only_has_placeholder_text_formats(),
                Err(_) => {
                    // No readable text. If there are no formats at all, it is a clear;
                    // otherwise something unsupported/custom remains — leave it alone.
                    return clipboard_format_count_is_zero();
                }
            }
        }

        if attempt + 1 < ATTEMPTS {
            std::thread::sleep(clipboard_retry_delay(attempt));
        }
    }

    false
}

#[cfg(target_os = "windows")]
fn clipboard_format_count_is_zero() -> bool {
    use windows::Win32::System::DataExchange::{
        CloseClipboard, CountClipboardFormats, OpenClipboard,
    };

    let opened = unsafe { OpenClipboard(None) };
    if opened.is_err() {
        return false;
    }
    let count = unsafe { CountClipboardFormats() };
    let _ = unsafe { CloseClipboard() };
    count == 0
}

#[cfg(not(target_os = "windows"))]
fn clipboard_format_count_is_zero() -> bool {
    true
}

/// True when every format currently on the clipboard is one of the text
/// placeholders Windows keeps or synthesizes for an empty-text write
/// (CF_TEXT, CF_OEMTEXT, CF_UNICODETEXT, CF_LOCALE). A private/custom format
/// alongside them means an app copied real content we simply cannot read.
#[cfg(target_os = "windows")]
fn clipboard_only_has_placeholder_text_formats() -> bool {
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EnumClipboardFormats, OpenClipboard,
    };

    const CF_TEXT: u32 = 1;
    const CF_OEMTEXT: u32 = 7;
    const CF_UNICODETEXT: u32 = 13;
    const CF_LOCALE: u32 = 16;

    if unsafe { OpenClipboard(None) }.is_err() {
        // Contention: never treat an unreadable clipboard as a clear.
        return false;
    }
    let mut only_placeholders = true;
    let mut format = unsafe { EnumClipboardFormats(0) };
    while format != 0 {
        if !matches!(format, CF_TEXT | CF_OEMTEXT | CF_UNICODETEXT | CF_LOCALE) {
            only_placeholders = false;
            break;
        }
        format = unsafe { EnumClipboardFormats(format) };
    }
    let _ = unsafe { CloseClipboard() };
    only_placeholders
}

#[cfg(not(target_os = "windows"))]
fn clipboard_only_has_placeholder_text_formats() -> bool {
    true
}

/// Supervise the native clipboard listener so capture never silently stops.
///
/// Tradeoffs (SOU-218):
/// - Forever-restart + capped backoff prefers availability over giving up.
/// - Watchdog recreates the monitor when the clipboard sequence advances but no
///   event arrives for [`STALE_LISTENER_AFTER`]. Capture can lag up to ~5–7s
///   after a silent death (sleep/Explorer), but avoids power-session Win32
///   surface area and toast spam.
/// - Every session start after the first catches up on a sequence the previous
///   session missed (restart window) or deferred (contended materialize), so
///   those copies are ingested late rather than lost.
/// - Consumer channel close stops the supervisor (process teardown).
#[cfg(target_os = "windows")]
fn run_native_listener(event_tx: tokio::sync::mpsc::UnboundedSender<ClipboardListenerEvent>) {
    spawn_listener_watchdog();

    let mut backoff = INITIAL_LISTENER_BACKOFF;
    let mut first_session = true;
    loop {
        set_capture_state(CAPTURE_STATE_RESTARTING);

        let mut monitor = match Monitor::new() {
            Ok(monitor) => monitor,
            Err(error) => {
                record_capture_error(format!("failed to create native listener: {error}"));
                std::thread::sleep(backoff);
                backoff = next_listener_backoff(backoff);
                continue;
            }
        };

        {
            let mut slot = LISTENER_SHUTDOWN.lock();
            *slot = Some(monitor.shutdown_channel());
        }

        let current_sequence =
            unsafe { windows::Win32::System::DataExchange::GetClipboardSequenceNumber() };
        if first_session {
            // Don't ingest whatever was on the clipboard before Cubby started;
            // only copies made while running belong in history.
            note_clipboard_event(current_sequence);
            first_session = false;
        } else if current_sequence != LAST_HANDLED_SEQUENCE.load(Ordering::SeqCst) {
            // A copy landed while the listener was down, or a previous
            // materialize was deferred under contention. Catch up now instead
            // of stamping the sequence handled and losing the copy.
            log::info!(
                "CLIPBOARD: Catching up on clipboard sequence {} missed during listener restart",
                current_sequence
            );
            if capture_clipboard_update(current_sequence, &event_tx).is_err() {
                record_capture_error("snapshot consumer stopped; capture supervisor exiting");
                set_capture_state(CAPTURE_STATE_STOPPED);
                return;
            }
        }
        set_capture_state(CAPTURE_STATE_LISTENING);
        log::info!("CLIPBOARD: Native WM_CLIPBOARDUPDATE listener started");

        let exit = run_listener_session(&mut monitor, &event_tx);
        *LISTENER_SHUTDOWN.lock() = None;
        drop(monitor);

        match exit {
            ListenerSessionExit::ConsumerGone => {
                record_capture_error("snapshot consumer stopped; capture supervisor exiting");
                set_capture_state(CAPTURE_STATE_STOPPED);
                return;
            }
            ListenerSessionExit::RestartRequested => {
                LISTENER_RESTART_COUNT.fetch_add(1, Ordering::SeqCst);
                log::warn!("CLIPBOARD: Listener session ended; recreating after short delay");
                // Watchdog-driven restarts should be quick; keep a small floor so we
                // do not tight-loop if shutdown races.
                std::thread::sleep(INITIAL_LISTENER_BACKOFF);
                backoff = INITIAL_LISTENER_BACKOFF;
            }
            ListenerSessionExit::Failed(error) => {
                LISTENER_RESTART_COUNT.fetch_add(1, Ordering::SeqCst);
                record_capture_error(format!("native listener failed: {error}"));
                std::thread::sleep(backoff);
                backoff = next_listener_backoff(backoff);
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn run_native_listener(_event_tx: tokio::sync::mpsc::UnboundedSender<ClipboardListenerEvent>) {
    set_capture_state(CAPTURE_STATE_STOPPED);
    record_capture_error("clipboard capture requires Windows");
}

fn materialize_clipboard_content() -> Option<(CapturedContent, Vec<CapturedFormat>)> {
    const ATTEMPTS: u32 = 10;

    for attempt in 0..ATTEMPTS {
        // Screenshot tools commonly expose both a bitmap and CF_HDROP for the
        // saved image. Treat that as an image in Cubby. If the advertised image
        // is still being rendered, do not let the easier file read mask it
        // immediately; retry the image for the complete bounded window.
        if clipboard_has_image_format() && clipboard_has_file_payload_format() {
            if let Ok(image) = read_clipboard_image_fast(attempt + 1 == ATTEMPTS) {
                return Some((captured_image(image), Vec::new()));
            }

            if attempt + 1 < ATTEMPTS {
                std::thread::sleep(clipboard_retry_delay(attempt));
                continue;
            }
            // The caller records this hybrid update as handled. Do not fall
            // through to text, where the path could be captured as a text clip.
            return None;
        }

        if let Ok(ctx) = ClipboardContext::new() {
            if let Ok(text) = ctx.get_text() {
                if let Some(content) = capture_text(text) {
                    let mut formats = Vec::new();
                    if let Ok(html) = ctx.get_html() {
                        if !html.is_empty() {
                            formats.push(CapturedFormat {
                                name: "html",
                                content: html.into_bytes(),
                            });
                        }
                    }
                    if let Ok(rtf) = ctx.get_rich_text() {
                        if !rtf.is_empty() {
                            formats.push(CapturedFormat {
                                name: "rtf",
                                content: rtf.into_bytes(),
                            });
                        }
                    }
                    return Some((content, formats));
                }
            }
        }

        if let Ok(image) = read_clipboard_image_fast(attempt + 1 == ATTEMPTS) {
            return Some((captured_image(image), Vec::new()));
        }

        if attempt + 1 < ATTEMPTS {
            std::thread::sleep(clipboard_retry_delay(attempt));
        }
    }

    None
}

fn captured_image(image: ClipboardImageRead) -> CapturedContent {
    CapturedContent::Image {
        png_bytes: image.png_bytes,
        width: image.width,
        height: image.height,
        hash: image.raw_hash,
        decode_ms: image.decode_ms,
        source_type: image.source_type,
    }
}

fn clipboard_retry_delay(attempt: u32) -> std::time::Duration {
    std::time::Duration::from_millis(1_u64 << attempt.min(6))
}

fn capture_text(text: String) -> Option<CapturedContent> {
    if text.is_empty() {
        return None;
    }

    let content = text.into_bytes();
    let preview = String::from_utf8_lossy(&content)
        .chars()
        .take(200)
        .collect::<String>();
    let hash = calculate_hash(&content);
    Some(CapturedContent::Text {
        content,
        preview,
        hash,
    })
}

fn read_clipboard_image_with_clipboard_rs(
    source_type: &'static str,
) -> Result<ClipboardImageRead, String> {
    let ctx = ClipboardContext::new().map_err(|e| e.to_string())?;
    let image = ctx.get_image().map_err(|e| e.to_string())?;
    let (width, height) = image.get_size();

    let dynamic_image = image.get_dynamic_image().map_err(|e| e.to_string())?;
    let raw_hash = calculate_hash(dynamic_image.as_bytes());

    let png_bytes = image
        .to_png()
        .map_err(|e| e.to_string())?
        .get_bytes()
        .to_vec();

    Ok(ClipboardImageRead {
        png_bytes,
        width,
        height,
        raw_hash,
        decode_ms: 0,
        source_type,
    })
}

#[cfg(target_os = "windows")]
fn read_registered_png_fast() -> Result<ClipboardImageRead, String> {
    use std::io::Cursor;

    let ctx = ClipboardContext::new().map_err(|e| e.to_string())?;
    let png_bytes = ctx.get_buffer("PNG").map_err(|e| e.to_string())?;
    let reader = image::io::Reader::new(Cursor::new(&png_bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let (width, height) = reader.into_dimensions().map_err(|e| e.to_string())?;

    Ok(ClipboardImageRead {
        raw_hash: calculate_hash(&png_bytes),
        png_bytes,
        width,
        height,
        decode_ms: 0,
        source_type: "registered-png",
    })
}

#[cfg(target_os = "windows")]
fn clipboard_has_registered_png() -> bool {
    use windows::core::PCWSTR;
    use windows::Win32::System::DataExchange::{
        IsClipboardFormatAvailable, RegisterClipboardFormatW,
    };

    let name: Vec<u16> = "PNG".encode_utf16().chain(std::iter::once(0)).collect();
    let format = unsafe { RegisterClipboardFormatW(PCWSTR(name.as_ptr())) };
    format != 0 && unsafe { IsClipboardFormatAvailable(format) }.is_ok()
}

fn read_clipboard_image_fast(allow_slow_fallback: bool) -> Result<ClipboardImageRead, String> {
    #[cfg(target_os = "windows")]
    if clipboard_has_registered_png() {
        match read_registered_png_fast() {
            Ok(image) => return Ok(image),
            Err(error) if !allow_slow_fallback => return Err(error),
            Err(_) => {}
        }
    }

    read_clipboard_image_with_clipboard_rs("clipboard-rs-image")
}

fn rgba_to_cf_dib(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, String> {
    const HEADER_SIZE: usize = 40;
    let row_bytes = (width as usize)
        .checked_mul(4)
        .ok_or_else(|| "clipboard image row is too large".to_string())?;
    let pixel_bytes = row_bytes
        .checked_mul(height as usize)
        .ok_or_else(|| "clipboard image is too large".to_string())?;
    if rgba.len() != pixel_bytes {
        return Err("clipboard image pixel buffer has an invalid length".to_string());
    }
    let width_i32 = i32::try_from(width).map_err(|_| "clipboard image is too wide".to_string())?;
    let height_i32 =
        i32::try_from(height).map_err(|_| "clipboard image is too tall".to_string())?;
    let image_size =
        u32::try_from(pixel_bytes).map_err(|_| "clipboard image is too large".to_string())?;

    let mut dib = Vec::with_capacity(HEADER_SIZE + pixel_bytes);
    dib.extend_from_slice(&(HEADER_SIZE as u32).to_le_bytes());
    dib.extend_from_slice(&width_i32.to_le_bytes());
    // Positive DIB heights are bottom-up and have the broadest compatibility
    // with older Win32 paste targets.
    dib.extend_from_slice(&height_i32.to_le_bytes());
    dib.extend_from_slice(&1_u16.to_le_bytes());
    dib.extend_from_slice(&32_u16.to_le_bytes());
    dib.extend_from_slice(&0_u32.to_le_bytes()); // BI_RGB
    dib.extend_from_slice(&image_size.to_le_bytes());
    dib.extend_from_slice(&0_i32.to_le_bytes());
    dib.extend_from_slice(&0_i32.to_le_bytes());
    dib.extend_from_slice(&0_u32.to_le_bytes());
    dib.extend_from_slice(&0_u32.to_le_bytes());

    for source_row in rgba.chunks_exact(row_bytes).rev() {
        for pixel in source_row.chunks_exact(4) {
            dib.push(pixel[2]);
            dib.push(pixel[1]);
            dib.push(pixel[0]);
            dib.push(pixel[3]);
        }
    }
    Ok(dib)
}

/// Put stored PNG bytes on the clipboard without recompressing them.
/// `clipboard-rs::set_image` encodes PNG a second time before producing a
/// bitmap, which costs several seconds for multi-megapixel screenshots in dev
/// builds. The original PNG remains the lossless representation while CF_DIB
/// is generated with a linear RGBA-to-BGRA conversion for traditional apps.
pub(crate) fn set_clipboard_image_png(png_bytes: &[u8]) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        let rgba = image::load_from_memory(png_bytes)
            .map_err(|e| e.to_string())?
            .into_rgba8();
        let dib = rgba_to_cf_dib(rgba.width(), rgba.height(), rgba.as_raw())?;
        let png_format = clipboard_win::register_format("PNG")
            .ok_or_else(|| "could not register the PNG clipboard format".to_string())?;
        let _clipboard = clipboard_win::Clipboard::new_attempts(10)
            .map_err(|e| format!("could not open clipboard: {e}"))?;
        clipboard_win::raw::empty().map_err(|e| format!("could not clear clipboard: {e}"))?;

        // CF_DIB is the compatibility format used by many traditional Windows
        // paste targets, so it must succeed before this write can be reported
        // as successful. Writing it first also avoids leaving a PNG-only
        // clipboard behind when the required format fails.
        clipboard_win::raw::set_without_clear(8, &dib)
            .map_err(|error| format!("could not set CF_DIB: {error}"))?;
        if let Err(error) = clipboard_win::raw::set_without_clear(png_format.get(), png_bytes) {
            log::warn!("CLIPBOARD: CF_DIB set succeeded but PNG failed: {error}");
        }
        Ok(())
    }

    #[cfg(not(target_os = "windows"))]
    {
        let image = RustImageData::from_bytes(png_bytes).map_err(|e| e.to_string())?;
        ClipboardContext::new()
            .and_then(|context| context.set_image(image))
            .map_err(|e| e.to_string())
    }
}

/// Relay only captures owned by a remote-control viewer, and never
/// sensitive-tagged content: a rewrite would strip the
/// `ExcludeClipboardContentFromMonitorProcessing` flag other monitors rely on.
fn should_relay_capture(relay_enabled: bool, sensitive: bool, exe_name: Option<&str>) -> bool {
    relay_enabled
        && !sensitive
        && exe_name.is_some_and(|exe| {
            crate::paste_engine::paste_strategy_for_process(exe)
                != crate::paste_engine::PasteStrategy::Standard
        })
}

/// Rewrite a capture that originated in a remote-control viewer back to the
/// clipboard under Cubby's ownership.
///
/// Remote viewers (NinjaOne's ncplayer, mstsc, ...) suppress clipboard updates
/// owned by their own process to avoid sync loops, so a copy inside one remote
/// session never reaches a second session on the same machine. A rewrite from a
/// foreign process (Cubby) is accepted by every viewer window and synced into
/// all sessions. Loop safety: the ignore hash swallows Cubby's own re-capture,
/// and [`LAST_RELAYED_HASH`] stops viewer echoes from relaying again.
fn relay_remote_capture(
    clip_type: &str,
    clip_content: &[u8],
    full_image_content: Option<&[u8]>,
    captured_formats: &[CapturedFormat],
    clip_hash: &str,
) {
    let image_content = if clip_type == "image" {
        let Some(png_bytes) = full_image_content else {
            return;
        };
        Some(png_bytes)
    } else {
        None
    };
    let mut contents = if image_content.is_some() {
        Vec::new()
    } else {
        vec![ClipboardContent::Text(
            String::from_utf8_lossy(clip_content).to_string(),
        )]
    };
    for format in captured_formats {
        match format.name {
            "html" => contents.push(ClipboardContent::Html(
                String::from_utf8_lossy(&format.content).to_string(),
            )),
            "rtf" => contents.push(ClipboardContent::Rtf(
                String::from_utf8_lossy(&format.content).to_string(),
            )),
            _ => {}
        }
    }

    {
        let mut last = LAST_RELAYED_HASH.lock();
        if last.as_deref() == Some(clip_hash) {
            return;
        }
        *last = Some(clip_hash.to_string());
    }
    set_ignore_hash(clip_hash.to_string());
    let set_result = if let Some(png_bytes) = image_content {
        set_clipboard_image_png(png_bytes)
    } else {
        ClipboardContext::new()
            .and_then(|context| context.set(contents))
            .map_err(|error| error.to_string())
    };
    match set_result {
        Ok(()) => log::info!("CLIPBOARD: Relayed remote-session capture under Cubby ownership"),
        Err(error) => {
            clear_ignore_hash_if_matches(clip_hash);
            let mut last = LAST_RELAYED_HASH.lock();
            if last.as_deref() == Some(clip_hash) {
                last.take();
            }
            log::warn!("CLIPBOARD: Failed to relay remote capture: {error}");
        }
    }
}

async fn process_clipboard_snapshot(
    app: AppHandle,
    db: Arc<Database>,
    snapshot: ClipboardSnapshot,
) {
    let started = std::time::Instant::now();
    let mut was_existing = false;
    let _guard = CLIPBOARD_SYNC.lock().await;

    let materialize_ms = snapshot.materialize_ms;
    let sequence = snapshot.sequence;
    let sensitive = snapshot.sensitive;
    let source_app_info = resolve_source_app_info(snapshot.source_app_identity);
    let captured_formats = snapshot.formats;
    let (clip_type, clip_content, clip_preview, _primary_hash, full_image_content, metadata) =
        match snapshot.content {
            CapturedContent::Text {
                content,
                preview,
                hash,
            } => {
                let format_names: Vec<&str> =
                    captured_formats.iter().map(|format| format.name).collect();
                (
                    "text",
                    content,
                    preview,
                    hash,
                    None,
                    (!format_names.is_empty())
                        .then(|| serde_json::json!({ "formats": format_names }).to_string()),
                )
            }
            CapturedContent::Image {
                png_bytes,
                width,
                height,
                hash,
                decode_ms,
                source_type,
            } => {
                let size_bytes = png_bytes.len();
                let preview_bytes = create_image_preview(&png_bytes).unwrap_or_default();
                log::debug!(
                    "CLIPBOARD: Materialized image sequence={} {}x{} source_type={} png_bytes={} decode_ms={}",
                    sequence,
                    width,
                    height,
                    source_type,
                    size_bytes,
                    decode_ms
                );
                (
                    "image",
                    preview_bytes,
                    "[Image]".to_string(),
                    hash,
                    Some(png_bytes),
                    Some(
                        serde_json::json!({
                            "width": width,
                            "height": height,
                            "format": "png",
                            "size_bytes": size_bytes
                        })
                        .to_string(),
                    ),
                )
            }
        };
    let hash_material = build_clip_hash_material(
        clip_type,
        full_image_content.as_deref().unwrap_or(&clip_content),
        captured_formats
            .iter()
            .map(|format| (format.name, format.content.as_slice())),
    );
    let clip_hash = calculate_hash(&hash_material);

    // Ignore our own clipboard writes. When a clip is pasted or reused from
    // Cubby, the paste path sets this ignore hash and already performed the
    // intended move-to-top bump. Re-capturing our own write here would relabel
    // the clip's source app (to Cubby) and re-bump its timestamp, which is what
    // made reused clips collapse to "1 second ago" with a "Cubby Clipboard"
    // source, so skip processing it entirely.
    {
        let mut lock = IGNORE_HASH.lock();
        if lock.as_deref() == Some(clip_hash.as_str()) {
            // Only consume the marker on a match. Clearing it for an
            // intermediate, non-matching snapshot would lose it before our own
            // write arrives, letting the self-paste be persisted after all.
            lock.take();
            log::info!("CLIPBOARD: Ignoring self-paste (own clipboard write)");
            return;
        }
    }

    // Source app info was captured at event time (before debounce) to avoid race conditions
    let (source_app, source_icon, exe_name, full_path, is_explicit_owner) = source_app_info;
    log::debug!(
        "CLIPBOARD: Source attribution available={} executable available={} explicit={}",
        source_app.is_some(),
        exe_name.is_some(),
        is_explicit_owner
    );

    // Check settings (cached via SettingsManager)
    use crate::settings_manager::SettingsManager;
    use tauri::Manager;
    let manager = app.state::<Arc<SettingsManager>>();
    let settings = manager.get();

    // Skipped captures must not leave an older clip as the clear-forget target.
    // Otherwise: copy note → skip password → auto-clear would delete the note.
    let discard_clear_target = || {
        *LAST_ACCEPTED_CAPTURE.lock() = None;
    };

    if settings.skip_sensitive && sensitive {
        log::info!("CLIPBOARD: Skipping content the source app marked as sensitive");
        discard_clear_target();
        return;
    }

    if settings.skip_likely_secrets && clip_type == "text" {
        if let Ok(text) = std::str::from_utf8(&clip_content) {
            if let Some(kind) = crate::secrets::classify_secret(text) {
                // Category only — never log the matched clipboard bytes.
                log::info!("CLIPBOARD: Skipping likely secret ({})", kind.as_str());
                discard_clear_target();
                return;
            }
        }
    }

    if settings.ignore_ghost_clips && !is_explicit_owner {
        log::info!("CLIPBOARD: Ignoring ghost clip (unknown owner)");
        discard_clear_target();
        return;
    }

    // Check if the app is in the ignore list (Case Insensitive)
    let is_ignored = |name: &str| {
        let name_lower = name.to_lowercase();
        settings
            .ignored_apps
            .iter()
            .any(|app| app.to_lowercase() == name_lower)
    };

    if let Some(ref path) = full_path {
        if is_ignored(path) {
            log::info!("CLIPBOARD: Ignoring content from configured application (path match)");
            discard_clear_target();
            return;
        }
    }

    if let Some(ref exe) = exe_name {
        if is_ignored(exe) {
            log::info!(
                "CLIPBOARD: Ignoring content from configured application (executable match)"
            );
            discard_clear_target();
            return;
        }
    }

    // Only accepted content participates in consecutive duplicate suppression.
    // An ignored application must not prevent the same content from being captured later.
    {
        let lock = LAST_STABLE_HASH.lock();
        if let Some(ref last_hash) = *lock {
            if last_hash == &clip_hash {
                return;
            }
        }
    }

    // DB Logic
    let pool = &db.pool;
    let storage_hash = db.crypto.keyed_hash(&hash_material);
    let encrypted_content = match db.crypto.encrypt(&clip_content) {
        Ok(content) => content,
        Err(error) => {
            log::error!("CLIPBOARD: Failed to encrypt captured content: {}", error);
            return;
        }
    };
    let encrypted_preview = match db.crypto.encrypt_text(&clip_preview) {
        Ok(preview) => preview,
        Err(error) => {
            log::error!("CLIPBOARD: Failed to encrypt captured preview: {}", error);
            return;
        }
    };
    let encrypted_source_app = match db.crypto.encrypt_optional_text(source_app.as_deref()) {
        Ok(value) => value,
        Err(error) => {
            log::error!("CLIPBOARD: Failed to encrypt source attribution: {}", error);
            return;
        }
    };
    let encrypted_source_icon = match db.crypto.encrypt_optional_text(source_icon.as_deref()) {
        Ok(value) => value,
        Err(error) => {
            log::error!("CLIPBOARD: Failed to encrypt source icon: {}", error);
            return;
        }
    };
    let encrypted_metadata = match db.crypto.encrypt_optional_text(metadata.as_deref()) {
        Ok(value) => value,
        Err(error) => {
            log::error!("CLIPBOARD: Failed to encrypt content metadata: {}", error);
            return;
        }
    };

    let db_lookup_started = std::time::Instant::now();
    let existing_uuid: Option<String> =
        sqlx::query_scalar::<_, String>(r#"SELECT uuid FROM clips WHERE content_hash = ?"#)
            .bind(&storage_hash)
            .fetch_optional(pool)
            .await
            .unwrap_or(None);
    let db_lookup_ms = db_lookup_started.elapsed().as_millis();

    let db_write_started = std::time::Instant::now();
    let (emitted_id, inserted_new) = if let Some(existing_id) = existing_uuid {
        was_existing = true;
        if clip_type == "image" {
            if let Err(error) = sqlx::query(
                r#"
                UPDATE clips
                SET created_at = CURRENT_TIMESTAMP,
                    is_deleted = 0,
                    source_app = ?,
                    source_icon = ?,
                    content = ?,
                    text_preview = ?,
                    metadata = ?,
                    is_thumbnail = 0
                WHERE uuid = ?
                "#,
            )
            .bind(&encrypted_source_app)
            .bind(&encrypted_source_icon)
            .bind(&encrypted_content)
            .bind(&encrypted_preview)
            .bind(encrypted_metadata.clone())
            .bind(&existing_id)
            .execute(pool)
            .await
            {
                log::error!(
                    "CLIPBOARD: Failed to update existing image clip {}: {}",
                    existing_id,
                    error
                );
                return;
            }

            if let Some(full_bytes) = &full_image_content {
                match persist_full_image_file(
                    &db.crypto,
                    &db.image_dir,
                    &existing_id,
                    full_bytes,
                ) {
                    Ok(file_path) => {
                        if let Err(error) = sqlx::query(
                            r#"
                            INSERT OR REPLACE INTO clip_images (clip_uuid, full_content, file_path, file_size, storage_kind, mime_type, created_at)
                            VALUES (?, x'', ?, ?, 'file', 'image/png', CURRENT_TIMESTAMP)
                            "#,
                        )
                        .bind(&existing_id)
                        .bind(&file_path)
                        .bind(full_bytes.len() as i64)
                        .execute(pool)
                        .await
                        {
                            log::error!(
                                "CLIPBOARD: Failed to index image file for existing clip {}: {}",
                                existing_id,
                                error
                            );
                            return;
                        }
                    }
                    Err(e) => {
                        log::error!(
                            "Failed to persist full image file for existing clip {}: {}",
                            existing_id,
                            e
                        );
                    }
                }
            }
        } else {
            if let Err(error) = sqlx::query(r#"UPDATE clips SET created_at = CURRENT_TIMESTAMP, is_deleted = 0, source_app = ?, source_icon = ? WHERE uuid = ?"#)
                .bind(&encrypted_source_app)
                .bind(&encrypted_source_icon)
                .bind(&existing_id)
                .execute(pool)
                .await
            {
                log::error!(
                    "CLIPBOARD: Failed to update existing text clip {}: {}",
                    existing_id,
                    error
                );
                return;
            }
        }
        (existing_id, false)
    } else {
        let clip_uuid = Uuid::new_v4().to_string();

        if let Err(error) = sqlx::query(
            r#"
            INSERT INTO clips (uuid, clip_type, content, text_preview, content_hash, folder_id, is_deleted, is_thumbnail, source_app, source_icon, metadata, ocr_status, created_at, last_accessed)
            VALUES (?, ?, ?, ?, ?, NULL, 0, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
            "#,
        )
        .bind(&clip_uuid)
        .bind(clip_type)
        .bind(&encrypted_content)
        .bind(&encrypted_preview)
        .bind(&storage_hash)
        .bind(false)
        .bind(&encrypted_source_app)
        .bind(&encrypted_source_icon)
        .bind(encrypted_metadata)
        .bind((clip_type == "image").then_some("pending"))
        .execute(pool)
        .await
        {
            log::error!(
                "CLIPBOARD: Failed to insert {} clip for sequence {}: {}",
                clip_type,
                sequence,
                error
            );
            return;
        }

        if clip_type == "image" {
            if let Some(full_bytes) = &full_image_content {
                match persist_full_image_file(
                    &db.crypto,
                    &db.image_dir,
                    &clip_uuid,
                    full_bytes,
                ) {
                    Ok(file_path) => {
                        if let Err(error) = sqlx::query(
                            r#"
                            INSERT OR REPLACE INTO clip_images (clip_uuid, full_content, file_path, file_size, storage_kind, mime_type, created_at)
                            VALUES (?, x'', ?, ?, 'file', 'image/png', CURRENT_TIMESTAMP)
                            "#,
                        )
                        .bind(&clip_uuid)
                        .bind(&file_path)
                        .bind(full_bytes.len() as i64)
                        .execute(pool)
                        .await
                        {
                            log::error!(
                                "CLIPBOARD: Failed to index image file for new clip {}: {}",
                                clip_uuid,
                                error
                            );
                            let _ = sqlx::query(r#"DELETE FROM clips WHERE uuid = ?"#)
                                .bind(&clip_uuid)
                                .execute(pool)
                                .await;
                            remove_full_image_file(&file_path);
                            return;
                        }
                    }
                    Err(e) => {
                        log::error!(
                            "Failed to persist full image file for new clip {}, dropping clip: {}",
                            clip_uuid,
                            e
                        );
                        let _ = sqlx::query(r#"DELETE FROM clips WHERE uuid = ?"#)
                            .bind(&clip_uuid)
                            .execute(pool)
                            .await;
                        return;
                    }
                }
            }
        }
        (clip_uuid, true)
    };
    let db_write_ms = db_write_started.elapsed().as_millis();

    // Durable background OCR is queued only after the image payload is safely
    // stored. Re-copying an image with missing OCR also gives it a fresh retry.
    if clip_type == "image" {
        if let Err(error) = crate::ocr_queue::enqueue(&db, &emitted_id).await {
            log::warn!("OCR: could not queue stored image: {error}");
        }
    }

    if let Err(error) = replace_clip_formats(pool, &db.crypto, &emitted_id, &captured_formats).await
    {
        log::error!("CLIPBOARD: Failed to persist auxiliary formats: {}", error);
        if inserted_new {
            let image_path: Option<String> =
                sqlx::query_scalar("SELECT file_path FROM clip_images WHERE clip_uuid = ?")
                    .bind(&emitted_id)
                    .fetch_optional(pool)
                    .await
                    .unwrap_or(None);
            match sqlx::query("DELETE FROM clips WHERE uuid = ?")
                .bind(&emitted_id)
                .execute(pool)
                .await
            {
                Ok(_) => crate::commands::remove_clip_image_files(
                    &db.image_dir,
                    image_path.into_iter().collect(),
                ),
                Err(cleanup_error) => log::error!(
                    "CLIPBOARD: Failed to roll back incomplete clip {}: {}",
                    emitted_id,
                    cleanup_error
                ),
            }
        }
        return;
    }

    *LAST_STABLE_HASH.lock() = Some(clip_hash.clone());

    let retention_deleted = match crate::commands::enforce_retention_in_pool(
        pool,
        settings.max_items,
        settings.auto_delete_days,
    )
    .await
    {
        Ok((deleted, image_paths)) => {
            crate::commands::remove_clip_image_files(&db.image_dir, image_paths);
            if deleted > 0 {
                log::info!(
                    "CLIPBOARD: Retention removed {} expired or overflow items",
                    deleted
                );
            }
            deleted
        }
        Err(error) => {
            log::error!("CLIPBOARD: Retention maintenance failed: {}", error);
            0
        }
    };

    if retention_deleted > 0 {
        db.search_index.invalidate();
    } else {
        db.search_index
            .upsert(&emitted_id, clip_type, &clip_content, &clip_preview, None);
    }

    // Remember this capture so a short-lived auto-clear can forget it (SOU-316).
    let credential_like = clip_type == "text"
        && std::str::from_utf8(&clip_content).is_ok_and(crate::secrets::looks_like_credential);
    *LAST_ACCEPTED_CAPTURE.lock() = Some(RecentCapture {
        uuid: emitted_id.clone(),
        captured_at: Instant::now(),
        credential_like,
    });

    // Re-announce remote-viewer captures so a second remote session can paste
    // them directly.
    if should_relay_capture(
        settings.remote_clipboard_relay,
        sensitive,
        exe_name.as_deref(),
    ) {
        relay_remote_capture(
            clip_type,
            &clip_content,
            full_image_content.as_deref(),
            &captured_formats,
            &clip_hash,
        );
    }

    let emit_started = Instant::now();
    let _ = app.emit(
        "clipboard-change",
        &serde_json::json!({
            "id": emitted_id,
            "content": clip_preview,
            "clip_type": clip_type,
            "source_app": source_app,
            "source_icon": source_icon,
            "created_at": chrono::Utc::now().to_rfc3339()
        }),
    );
    let emit_ms = emit_started.elapsed().as_millis();

    log::info!(
        "[perf][clipboard_ingest] sequence={} type={} existing={} full_bytes={} thumb_bytes={} materialize_ms={} db_lookup_ms={} db_write_ms={} emit_ms={} total_ms={}",
        sequence,
        clip_type,
        was_existing,
        full_image_content.as_ref().map(|v| v.len()).unwrap_or(0),
        if clip_type == "image" { clip_content.len() } else { 0 },
        materialize_ms,
        db_lookup_ms,
        db_write_ms,
        emit_ms,
        started.elapsed().as_millis()
    );
}

/// Forget the last capture when the clipboard is cleared shortly afterward.
///
/// Password-manager browser extensions copy into chrome.exe/msedge.exe (so the
/// ignored-apps list cannot help) and almost always empty the clipboard a few
/// seconds later. Only empty/clear events trigger this — never a new non-empty
/// copy. Pinned items are never removed.
async fn process_clipboard_clear(app: AppHandle, db: Arc<Database>, sequence: u32) {
    use crate::settings_manager::SettingsManager;
    use tauri::Manager;

    let manager = app.state::<Arc<SettingsManager>>();
    let settings = manager.get();
    if !settings.forget_on_clipboard_clear {
        return;
    }

    let recent = {
        let mut lock = LAST_ACCEPTED_CAPTURE.lock();
        let should_forget = lock.as_ref().is_some_and(|recent| {
            should_forget_recent_capture(
                recent.captured_at,
                Instant::now(),
                CLIPBOARD_CLEAR_FORGET_WINDOW,
            )
        });
        if should_forget {
            lock.take()
        } else {
            // Outside the window (or nothing tracked): drop any stale marker.
            lock.take();
            None
        }
    };

    let Some(recent) = recent else {
        log::debug!(
            "CLIPBOARD: Clear sequence {} with no recent capture to forget",
            sequence
        );
        return;
    };

    if !recent.credential_like {
        log::info!(
            "CLIPBOARD: Clear sequence {} — keeping capture {} (not credential-shaped)",
            sequence,
            recent.uuid
        );
        return;
    }

    let restore_marker = |recent: RecentCapture| {
        let mut lock = LAST_ACCEPTED_CAPTURE.lock();
        // Only restore if nothing newer was captured while we held the marker.
        if lock.is_none() {
            *lock = Some(recent);
        }
    };

    let _guard = CLIPBOARD_SYNC.lock().await;
    let pool = &db.pool;

    let row: Option<(i64, i64)> = sqlx::query_as(
        r#"
        SELECT is_pinned, is_deleted
        FROM clips
        WHERE uuid = ?
        "#,
    )
    .bind(&recent.uuid)
    .fetch_optional(pool)
    .await
    .unwrap_or(None);

    let Some((is_pinned, is_deleted)) = row else {
        log::debug!(
            "CLIPBOARD: Clear sequence {} — recent capture {} already gone",
            sequence,
            recent.uuid
        );
        return;
    };

    if is_pinned != 0 {
        log::info!(
            "CLIPBOARD: Clear sequence {} — keeping pinned capture {}",
            sequence,
            recent.uuid
        );
        // Keep tracking so a later clear after unpin is not required; pinned
        // items stay forever under this policy.
        return;
    }
    if is_deleted != 0 {
        return;
    }

    let mut transaction = match pool.begin().await {
        Ok(tx) => tx,
        Err(error) => {
            log::error!("CLIPBOARD: Failed to begin clear-forget transaction: {error}");
            restore_marker(recent);
            return;
        }
    };

    let file_path: Option<String> =
        sqlx::query_scalar(r#"SELECT file_path FROM clip_images WHERE clip_uuid = ?"#)
            .bind(&recent.uuid)
            .fetch_optional(&mut *transaction)
            .await
            .unwrap_or(None);

    // Guarded delete: re-check is_pinned in the same statement so a pin that
    // lands between the earlier SELECT and this DELETE cannot leave orphan
    // file/index cleanup for a still-present clip (TOCTOU).
    let deleted = match sqlx::query(r#"DELETE FROM clips WHERE uuid = ? AND is_pinned = 0"#)
        .bind(&recent.uuid)
        .execute(&mut *transaction)
        .await
    {
        Ok(result) => result.rows_affected(),
        Err(error) => {
            log::error!(
                "CLIPBOARD: Failed to forget cleared capture {}: {error}",
                recent.uuid
            );
            restore_marker(recent);
            return;
        }
    };

    if deleted == 0 {
        log::info!(
            "CLIPBOARD: Clear sequence {} — capture {} not deleted (pinned or already gone)",
            sequence,
            recent.uuid
        );
        // No restore: either pinned now or already removed. Either way the
        // clear-forget target should not fire again for this uuid.
        return;
    }

    if let Err(error) = transaction.commit().await {
        log::error!(
            "CLIPBOARD: Failed to commit clear-forget for {}: {error}",
            recent.uuid
        );
        restore_marker(recent);
        return;
    }

    crate::commands::remove_clip_image_files(&db.image_dir, file_path.into_iter().collect());
    db.search_index.remove(&recent.uuid);
    // The forgotten clip's hash must not suppress a deliberate re-copy later.
    reset_capture_dedup();

    log::info!(
        "CLIPBOARD: Forgot capture {} after clipboard clear (sequence {}, within {}s window)",
        recent.uuid,
        sequence,
        CLIPBOARD_CLEAR_FORGET_WINDOW.as_secs()
    );

    let _ = app.emit(
        "clipboard-change",
        &serde_json::json!({
            "id": recent.uuid,
            "forgotten_on_clear": true,
        }),
    );
}
pub(crate) fn calculate_hash(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    let result = hasher.finalize();
    format!("{:x}", result)
}

/// Build the stable identity used for capture deduplication and self-write
/// suppression. Auxiliary representations are part of rich text identity, but
/// an image is identified by its stored bitmap rather than a screenshot tool's
/// saved file path.
pub(crate) fn build_clip_hash_material<'a>(
    clip_type: &str,
    primary_content: &[u8],
    formats: impl IntoIterator<Item = (&'a str, &'a [u8])>,
) -> Vec<u8> {
    let mut material = Vec::new();
    material.extend_from_slice(clip_type.as_bytes());
    material.push(0);
    material.extend_from_slice(primary_content);
    if clip_type != "image" {
        for (name, content) in formats {
            material.push(0);
            material.extend_from_slice(name.as_bytes());
            material.push(0);
            material.extend_from_slice(content);
        }
    }
    material
}

pub(crate) async fn replace_clip_formats(
    pool: &sqlx::SqlitePool,
    crypto: &crate::crypto::CryptoManager,
    clip_uuid: &str,
    formats: &[CapturedFormat],
) -> Result<(), String> {
    let mut transaction = pool.begin().await.map_err(|e| e.to_string())?;
    sqlx::query("DELETE FROM clip_formats WHERE clip_uuid = ?")
        .bind(clip_uuid)
        .execute(&mut *transaction)
        .await
        .map_err(|e| e.to_string())?;
    for format in formats {
        sqlx::query("INSERT INTO clip_formats (clip_uuid, format, content) VALUES (?, ?, ?)")
            .bind(clip_uuid)
            .bind(format.name)
            .bind(crypto.encrypt(&format.content)?)
            .execute(&mut *transaction)
            .await
            .map_err(|e| e.to_string())?;
    }
    transaction.commit().await.map_err(|e| e.to_string())?;
    Ok(())
}

pub fn create_image_preview(png_bytes: &[u8]) -> Result<Vec<u8>, String> {
    let image = image::load_from_memory(png_bytes).map_err(|e| e.to_string())?;
    let preview = image.thumbnail(320, 220);
    let mut bytes = std::io::Cursor::new(Vec::new());
    preview
        .write_to(&mut bytes, image::ImageOutputFormat::Png)
        .map_err(|e| e.to_string())?;
    Ok(bytes.into_inner())
}

pub fn persist_full_image_file(
    crypto: &crate::crypto::CryptoManager,
    image_dir: &std::path::Path,
    clip_uuid: &str,
    png_bytes: &[u8],
) -> Result<String, String> {
    std::fs::create_dir_all(image_dir).map_err(|e| e.to_string())?;
    let file_path = image_dir.join(format!("{}.cubby", clip_uuid));
    let encrypted = crypto.encrypt(png_bytes)?;
    std::fs::write(&file_path, encrypted).map_err(|e| e.to_string())?;
    Ok(file_path.to_string_lossy().to_string())
}

pub fn read_full_image_file(
    crypto: &crate::crypto::CryptoManager,
    file_path: &str,
) -> Result<Vec<u8>, String> {
    let encrypted = std::fs::read(file_path).map_err(|e| e.to_string())?;
    crypto.decrypt(&encrypted)
}

pub fn remove_full_image_file(file_path: &str) {
    if let Err(e) = std::fs::remove_file(file_path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            log::warn!("Failed to delete a stored clipboard image: {}", e);
        }
    }
}

#[cfg(target_os = "windows")]
fn get_clipboard_owner_identity() -> Option<SourceAppIdentity> {
    unsafe {
        let (hwnd, is_explicit) = match GetClipboardOwner() {
            Ok(h) if !h.0.is_null() => (h, true),
            Err(e) => {
                log::info!(
                    "CLIPBOARD: GetClipboardOwner failed: {:?}, falling back to foreground window",
                    e
                );
                (GetForegroundWindow(), false)
            }
            Ok(_) => {
                log::info!(
                    "CLIPBOARD: GetClipboardOwner returned null, falling back to foreground window"
                );
                (GetForegroundWindow(), false)
            }
        };

        if hwnd.0.is_null() {
            return None;
        }

        let mut process_id = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut process_id));

        if process_id == 0 {
            return None;
        }

        Some(SourceAppIdentity {
            process_id,
            is_explicit_owner: is_explicit,
        })
    }
}

#[cfg(not(target_os = "windows"))]
fn get_clipboard_owner_identity() -> Option<SourceAppIdentity> {
    None
}

#[cfg(target_os = "windows")]
fn resolve_source_app_info(identity: Option<SourceAppIdentity>) -> SourceAppInfo {
    unsafe {
        let Some(identity) = identity else {
            return (None, None, None, None, false);
        };

        let process_handle = match OpenProcess(
            PROCESS_QUERY_INFORMATION | PROCESS_VM_READ,
            false,
            identity.process_id,
        ) {
            Ok(h) => h,
            Err(_) => return (None, None, None, None, false),
        };

        let mut name_buffer = [0u16; MAX_PATH as usize];
        let name_size = GetModuleBaseNameW(process_handle, None, &mut name_buffer);
        let exe_name = if name_size > 0 {
            String::from_utf16_lossy(&name_buffer[..name_size as usize])
        } else {
            String::new()
        };

        let mut path_buffer = [0u16; MAX_PATH as usize];
        let path_size = GetModuleFileNameExW(Some(process_handle), None, &mut path_buffer);
        let (app_name, app_icon, full_path) = if path_size > 0 {
            let full_path_str = String::from_utf16_lossy(&path_buffer[..path_size as usize]);

            let desc = get_app_description(&full_path_str);
            let final_name = if let Some(d) = desc {
                Some(d)
            } else {
                if !exe_name.is_empty() {
                    Some(exe_name.clone())
                } else {
                    None
                }
            };

            let icon = extract_icon(&full_path_str);
            (final_name, icon, Some(full_path_str))
        } else {
            (
                if !exe_name.is_empty() {
                    Some(exe_name.clone())
                } else {
                    None
                },
                None,
                None,
            )
        };

        let exe_val = if !exe_name.is_empty() {
            Some(exe_name)
        } else {
            None
        };
        (
            app_name,
            app_icon,
            exe_val,
            full_path,
            identity.is_explicit_owner,
        )
    }
}

#[cfg(not(target_os = "windows"))]
fn resolve_source_app_info(_identity: Option<SourceAppIdentity>) -> SourceAppInfo {
    (None, None, None, None, false)
}

#[cfg(target_os = "windows")]
unsafe fn get_app_description(path: &str) -> Option<String> {
    use std::ffi::c_void;

    let wide_path: Vec<u16> = OsStr::new(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let size = GetFileVersionInfoSizeW(windows::core::PCWSTR(wide_path.as_ptr()), None);
    if size == 0 {
        return None;
    }

    let mut data = vec![0u8; size as usize];
    if GetFileVersionInfoW(
        windows::core::PCWSTR(wide_path.as_ptr()),
        Some(0),
        size,
        data.as_mut_ptr() as *mut _,
    )
    .is_err()
    {
        return None;
    }

    let mut lang_ptr: *mut c_void = std::ptr::null_mut();
    let mut lang_len: u32 = 0;

    let translation_query = OsStr::new("\\VarFileInfo\\Translation")
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<u16>>();

    if !VerQueryValueW(
        data.as_ptr() as *const _,
        windows::core::PCWSTR(translation_query.as_ptr()),
        &mut lang_ptr,
        &mut lang_len,
    )
    .as_bool()
    {
        return None;
    }

    if lang_len < 4 {
        return None;
    }

    let pairs = std::slice::from_raw_parts(lang_ptr as *const u16, (lang_len / 2) as usize);
    let num_pairs = (lang_len / 4) as usize;

    let mut lang_code = pairs[0];
    let mut charset_code = pairs[1];

    for i in 0..num_pairs {
        let code = pairs[i * 2];
        let charset = pairs[i * 2 + 1];

        if code == 0x0804 {
            lang_code = code;
            charset_code = charset;
        }
    }

    let keys = ["FileDescription", "ProductName"];

    for key in keys {
        let query_str = format!(
            "\\StringFileInfo\\{:04x}{:04x}\\{}",
            lang_code, charset_code, key
        );
        let query = OsStr::new(&query_str)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>();

        let mut desc_ptr: *mut c_void = std::ptr::null_mut();
        let mut desc_len: u32 = 0;

        if VerQueryValueW(
            data.as_ptr() as *const _,
            windows::core::PCWSTR(query.as_ptr()),
            &mut desc_ptr,
            &mut desc_len,
        )
        .as_bool()
        {
            let desc = std::slice::from_raw_parts(desc_ptr as *const u16, desc_len as usize);
            let len = if desc.last() == Some(&0) {
                desc.len() - 1
            } else {
                desc.len()
            };
            if len > 0 {
                return Some(String::from_utf16_lossy(&desc[..len]));
            }
        }
    }

    None
}

#[cfg(target_os = "windows")]
unsafe fn extract_icon(path: &str) -> Option<String> {
    use image::ImageEncoder;

    let wide_path: Vec<u16> = OsStr::new(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut shfi = SHFILEINFOW::default();

    SHGetFileInfoW(
        windows::core::PCWSTR(wide_path.as_ptr()),
        windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_NORMAL,
        Some(&mut shfi as *mut _),
        std::mem::size_of::<SHFILEINFOW>() as u32,
        SHGFI_ICON | SHGFI_LARGEICON | SHGFI_USEFILEATTRIBUTES,
    );

    if shfi.hIcon.is_invalid() {
        return None;
    }

    let icon = shfi.hIcon;
    struct IconGuard(windows::Win32::UI::WindowsAndMessaging::HICON);
    impl Drop for IconGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = DestroyIcon(self.0);
            }
        }
    }
    let _guard = IconGuard(icon);

    let mut icon_info = ICONINFO::default();
    if GetIconInfo(icon, &mut icon_info).is_err() {
        return None;
    }

    struct BitmapGuard(HBITMAP);
    impl Drop for BitmapGuard {
        fn drop(&mut self) {
            unsafe {
                if !self.0.is_invalid() {
                    let _ = DeleteObject(self.0.into());
                }
            }
        }
    }
    let _bm_mask = BitmapGuard(icon_info.hbmMask);
    let _bm_color = BitmapGuard(icon_info.hbmColor);

    let mut bm = BITMAP::default();
    if GetObjectW(
        icon_info.hbmMask.into(),
        std::mem::size_of::<BITMAP>() as i32,
        Some(&mut bm as *mut _ as *mut _),
    ) == 0
    {
        return None;
    }

    let width = bm.bmWidth;
    let height = if !icon_info.hbmColor.is_invalid() {
        bm.bmHeight
    } else {
        bm.bmHeight / 2
    };

    let screen_dc = GetDC(None);
    let mem_dc = CreateCompatibleDC(Some(screen_dc));
    let mem_bm = CreateCompatibleBitmap(screen_dc, width, height);

    let old_obj = SelectObject(mem_dc, mem_bm.into());

    let _ = DrawIconEx(mem_dc, 0, 0, icon, width, height, 0, None, DI_NORMAL);

    let bi = BITMAPINFOHEADER {
        biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
        biWidth: width,
        biHeight: -height,
        biPlanes: 1,
        biBitCount: 32,
        biCompression: BI_RGB.0,
        ..Default::default()
    };

    let mut pixels = vec![0u8; (width * height * 4) as usize];

    GetDIBits(
        mem_dc,
        mem_bm,
        0,
        height as u32,
        Some(pixels.as_mut_ptr() as *mut _),
        &mut BITMAPINFO {
            bmiHeader: bi,
            ..Default::default()
        },
        DIB_RGB_COLORS,
    );

    SelectObject(mem_dc, old_obj);
    let _ = DeleteDC(mem_dc);
    let _ = DeleteObject(mem_bm.into());
    let _ = ReleaseDC(None, screen_dc);

    for chunk in pixels.chunks_exact_mut(4) {
        let b = chunk[0];
        let r = chunk[2];
        chunk[0] = r;
        chunk[2] = b;
    }

    let mut png_data = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new(&mut png_data);
    encoder
        .write_image(
            &pixels,
            width as u32,
            height as u32,
            image::ColorType::Rgba8,
        )
        .ok()?;

    Some(BASE64.encode(&png_data))
}

#[cfg(test)]
mod tests {
    use super::{
        build_clip_hash_material, calculate_hash, capture_state_name, capture_text,
        clear_ignore_hash_if_matches, clipboard_retry_delay, next_listener_backoff, rgba_to_cf_dib,
        set_ignore_hash, should_forget_recent_capture, should_relay_capture, CapturedContent,
        CAPTURE_STATE_LISTENING, CAPTURE_STATE_RESTARTING, CAPTURE_STATE_STOPPED,
        CLIPBOARD_CLEAR_FORGET_WINDOW, IGNORE_HASH,
    };
    use std::time::{Duration, Instant};

    #[test]
    fn relays_only_remote_viewer_captures() {
        assert!(should_relay_capture(true, false, Some("ncplayer.exe")));
        assert!(should_relay_capture(true, false, Some("mstsc.exe")));
        assert!(!should_relay_capture(true, false, Some("notepad.exe")));
        assert!(!should_relay_capture(true, false, None));
    }

    #[test]
    fn relay_respects_setting_and_sensitive_flag() {
        assert!(!should_relay_capture(false, false, Some("ncplayer.exe")));
        assert!(!should_relay_capture(true, true, Some("ncplayer.exe")));
    }

    #[test]
    fn forget_window_accepts_clears_within_bound() {
        let captured = Instant::now();
        let cleared = captured + Duration::from_secs(30);
        assert!(should_forget_recent_capture(
            captured,
            cleared,
            CLIPBOARD_CLEAR_FORGET_WINDOW
        ));
        assert!(should_forget_recent_capture(
            captured,
            captured + CLIPBOARD_CLEAR_FORGET_WINDOW,
            CLIPBOARD_CLEAR_FORGET_WINDOW
        ));
    }

    #[test]
    fn forget_window_rejects_late_clears() {
        let captured = Instant::now();
        let cleared = captured + CLIPBOARD_CLEAR_FORGET_WINDOW + Duration::from_millis(1);
        assert!(!should_forget_recent_capture(
            captured,
            cleared,
            CLIPBOARD_CLEAR_FORGET_WINDOW
        ));
    }

    #[test]
    fn forget_window_rejects_inverted_timestamps() {
        let captured = Instant::now();
        let cleared = captured - Duration::from_secs(1);
        assert!(!should_forget_recent_capture(
            captured,
            cleared,
            CLIPBOARD_CLEAR_FORGET_WINDOW
        ));
    }

    #[test]
    fn capture_text_preserves_exact_whitespace() {
        let original = "  copied text\r\nwith trailing space  ".to_string();
        let captured = capture_text(original.clone()).expect("text should be captured");

        match captured {
            CapturedContent::Text {
                content,
                preview,
                hash,
            } => {
                assert_eq!(content, original.as_bytes());
                assert_eq!(preview, original);
                assert_eq!(hash, calculate_hash(original.as_bytes()));
            }
            CapturedContent::Image { .. } => panic!("expected text"),
        }
    }

    #[test]
    fn capture_text_ignores_only_truly_empty_content() {
        assert!(capture_text(String::new()).is_none());
        assert!(capture_text("   ".to_string()).is_some());
    }

    #[test]
    fn clipboard_contention_backoff_is_bounded() {
        let delays = (0..10)
            .map(|attempt| clipboard_retry_delay(attempt).as_millis())
            .collect::<Vec<_>>();

        assert_eq!(delays, vec![1, 2, 4, 8, 16, 32, 64, 64, 64, 64]);
        assert_eq!(delays.iter().sum::<u128>(), 319);
        assert_eq!(delays[..9].iter().sum::<u128>(), 255);
    }

    #[test]
    fn image_identity_ignores_auxiliary_file_path() {
        let first = build_clip_hash_material(
            "image",
            b"same pixels",
            [("files", b"[\"C:/shots/first.png\"]".as_slice())],
        );
        let second = build_clip_hash_material(
            "image",
            b"same pixels",
            [("files", b"[\"D:/archive/second.png\"]".as_slice())],
        );

        assert_eq!(first, second);
    }

    #[test]
    fn non_image_identity_preserves_auxiliary_formats() {
        let html = build_clip_hash_material(
            "text",
            b"same text",
            [("html", b"<b>same text</b>".as_slice())],
        );
        let plain = build_clip_hash_material("text", b"same text", []);

        assert_ne!(html, plain);
    }

    #[test]
    fn cf_dib_conversion_writes_bottom_up_bgra_pixels() {
        let rgba = [
            1, 2, 3, 4, 5, 6, 7, 8, // top row
            9, 10, 11, 12, 13, 14, 15, 16, // bottom row
        ];
        let dib = rgba_to_cf_dib(2, 2, &rgba).unwrap();

        assert_eq!(&dib[0..4], &40_u32.to_le_bytes());
        assert_eq!(&dib[4..8], &2_i32.to_le_bytes());
        assert_eq!(&dib[8..12], &2_i32.to_le_bytes());
        assert_eq!(
            &dib[40..],
            &[11, 10, 9, 12, 15, 14, 13, 16, 3, 2, 1, 4, 7, 6, 5, 8]
        );
    }

    #[test]
    fn failed_write_cleanup_only_clears_its_own_ignore_hash() {
        set_ignore_hash("expected".to_string());
        clear_ignore_hash_if_matches("different");
        assert_eq!(IGNORE_HASH.lock().as_deref(), Some("expected"));

        clear_ignore_hash_if_matches("expected");
        assert!(IGNORE_HASH.lock().is_none());
    }

    #[test]
    fn listener_restart_backoff_doubles_and_caps() {
        assert_eq!(
            next_listener_backoff(Duration::from_millis(500)),
            Duration::from_secs(1)
        );
        assert_eq!(
            next_listener_backoff(Duration::from_secs(16)),
            Duration::from_secs(30)
        );
        assert_eq!(
            next_listener_backoff(Duration::from_secs(30)),
            Duration::from_secs(30)
        );
    }

    #[test]
    fn capture_state_names_are_stable_for_diagnostics() {
        assert_eq!(capture_state_name(CAPTURE_STATE_LISTENING), "listening");
        assert_eq!(capture_state_name(CAPTURE_STATE_RESTARTING), "restarting");
        assert_eq!(capture_state_name(CAPTURE_STATE_STOPPED), "stopped");
    }
}
