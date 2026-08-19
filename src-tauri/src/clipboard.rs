use tauri::{AppHandle, Emitter};
// Import functions directly from the crate root
use crate::database::Database;
#[cfg(target_os = "windows")]
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use clipboard_rs::common::RustImage;
#[cfg(not(target_os = "windows"))]
use clipboard_rs::common::RustImageData;
// Windows relays through raw Win32 so content and its sensitive marker publish
// under one clipboard handle, so the typed content enum is portable-path only.
#[cfg(not(target_os = "windows"))]
use clipboard_rs::ClipboardContent;
use clipboard_rs::{Clipboard, ClipboardContext};
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
#[cfg(target_os = "windows")]
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use uuid::Uuid;
#[cfg(target_os = "windows")]
use windows::Win32::Foundation::CloseHandle;
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
/// Hash of a clipboard write Cubby made itself, with the time it was written.
/// Consumed by the capture of that write so a self-paste is not re-recorded.
static IGNORE_HASH: Lazy<parking_lot::Mutex<Option<(String, Instant)>>> =
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
/// How long a self-write marker stays valid. The capture of our own write
/// normally arrives within milliseconds, so this is generous. It exists because
/// an unbounded marker is never cleaned up when that capture never arrives at
/// all -- the read lost every race, or a remote client rewrote the clipboard
/// before we could read it -- and a stale marker silently swallows the next
/// legitimate copy of the same content.
const IGNORE_HASH_TTL: Duration = Duration::from_secs(5);
#[cfg(target_os = "windows")]
const CF_DIB_FORMAT: u32 = 8;

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

mod forget_on_clear;
use forget_on_clear::ForgetClipLookup;

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
/// How many times one clipboard sequence may be deferred under contention
/// before the copy is written off. Each deferral costs roughly
/// `STALE_LISTENER_AFTER` plus a listener restart, so this bounds a stuck
/// clipboard owner to about twelve seconds of retrying instead of forever.
const MAX_DEFERRALS_PER_SEQUENCE: u32 = 3;

static CAPTURE_STATE: AtomicU8 = AtomicU8::new(CAPTURE_STATE_STOPPED);
static LAST_CLIPBOARD_EVENT_UNIX_MS: AtomicU64 = AtomicU64::new(0);
static LAST_HANDLED_SEQUENCE: AtomicU32 = AtomicU32::new(0);
static LISTENER_RESTART_COUNT: AtomicU64 = AtomicU64::new(0);
/// The sequence currently being retried, and how many times it has been
/// deferred. Only ever touched from the listener thread.
static DEFERRED_SEQUENCE: AtomicU32 = AtomicU32::new(0);
static DEFERRED_ATTEMPTS: AtomicU32 = AtomicU32::new(0);
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
    *lock = Some((hash, Instant::now()));
}

pub(crate) fn clear_ignore_hash_if_matches(hash: &str) {
    let mut lock = IGNORE_HASH.lock();
    if lock.as_ref().is_some_and(|(marked, _)| marked == hash) {
        lock.take();
    }
}

/// Whether a self-write marker still applies to `hash`.
///
/// A marker that has outlived `ttl` is treated as absent: the write it
/// described was never observed, and honouring it would drop a real copy.
fn ignore_marker_applies(
    marker: Option<&(String, Instant)>,
    hash: &str,
    now: Instant,
    ttl: Duration,
) -> bool {
    marker.is_some_and(|(marked, marked_at)| {
        marked == hash
            && now
                .checked_duration_since(*marked_at)
                .is_some_and(|elapsed| elapsed <= ttl)
    })
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
#[derive(Debug, PartialEq, Eq)]
enum CaptureAttempt {
    /// A snapshot or clear event was queued (or the payload is unsupported);
    /// the sequence was marked handled.
    Handled,
    /// Supported content is present but every clipboard open lost the race.
    /// The sequence stays unhandled so the watchdog retries it.
    Deferred,
}

/// How many times one capture may restart after the clipboard sequence moves
/// mid-read. Each restart re-collects metadata and payload together.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
const MAX_SEQUENCE_BIND_ATTEMPTS: u32 = 3;

/// The clipboard advanced while this capture was still reading. Metadata from
/// the old sequence must not be paired with the new payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SequenceBindError {
    Changed { from: u32, to: u32 },
}

/// Source/sensitivity collected before the payload is materialized.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
struct CapturePrivacyMetadata {
    sensitive: bool,
}

/// One capture whose metadata and payload were read under the same sequence.
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(not(test), allow(dead_code))]
enum SequenceBoundCapture<T> {
    Bound {
        sequence: u32,
        metadata: CapturePrivacyMetadata,
        payload: T,
    },
    Changed {
        from: u32,
        to: u32,
    },
}

/// Collect metadata, then payload, and keep the pair only if the clipboard
/// sequence never moved. This is the test seam for SBS-767.
#[cfg_attr(not(test), allow(dead_code))]
fn bind_capture_to_sequence<S, M, P, T>(
    read_sequence: S,
    collect_metadata: M,
    collect_payload: P,
) -> SequenceBoundCapture<T>
where
    S: Fn() -> u32,
    M: FnOnce() -> CapturePrivacyMetadata,
    P: FnOnce() -> T,
{
    let start = read_sequence();
    let metadata = collect_metadata();
    let after_metadata = read_sequence();
    if after_metadata != start {
        return SequenceBoundCapture::Changed {
            from: start,
            to: after_metadata,
        };
    }
    let payload = collect_payload();
    let after_payload = read_sequence();
    if after_payload != start {
        return SequenceBoundCapture::Changed {
            from: start,
            to: after_payload,
        };
    }
    SequenceBoundCapture::Bound {
        sequence: start,
        metadata,
        payload,
    }
}

/// Persist only when the sequence is still the one we bound. A later copy must
/// not inherit this snapshot's privacy flags.
fn persist_if_sequence_holds<T>(
    expected: u32,
    current: u32,
    snapshot: T,
) -> Result<T, SequenceBindError> {
    if current == expected {
        Ok(snapshot)
    } else {
        Err(SequenceBindError::Changed {
            from: expected,
            to: current,
        })
    }
}

/// Retry a delayed materialize, but abort as soon as the sequence moves so a
/// later (possibly sensitive) payload is never kept with earlier metadata.
fn materialize_with_sequence_guard<F, T>(
    expected: u32,
    read_sequence: impl Fn() -> u32,
    mut attempt: F,
    max_attempts: u32,
) -> Result<Option<T>, SequenceBindError>
where
    F: FnMut(u32) -> Option<T>,
{
    for index in 0..max_attempts {
        let current = read_sequence();
        if current != expected {
            return Err(SequenceBindError::Changed {
                from: expected,
                to: current,
            });
        }
        if let Some(value) = attempt(index) {
            let current = read_sequence();
            if current != expected {
                return Err(SequenceBindError::Changed {
                    from: expected,
                    to: current,
                });
            }
            return Ok(Some(value));
        }
    }
    let current = read_sequence();
    if current != expected {
        return Err(SequenceBindError::Changed {
            from: expected,
            to: current,
        });
    }
    Ok(None)
}

/// Try to materialize and queue the clipboard content behind `sequence`.
///
/// The sequence is marked handled only after a successful materialize (or a
/// confirmed clear / unsupported payload). A contended read leaves it
/// unhandled: the watchdog then sees it stale after [`STALE_LISTENER_AFTER`]
/// and restarts the listener, whose session start retries this capture instead
/// of silently dropping the copy.
///
/// Count a deferral against `sequence` and decide whether it earns another try.
///
/// Returns the attempt count to remember, and `Handled` once the sequence has
/// used up `max_attempts`. Without that ceiling a clipboard owner that never
/// releases the lock keeps the sequence permanently unhandled, and the watchdog
/// restarts the listener every few seconds for the rest of the session.
#[cfg(target_os = "windows")]
fn deferral_decision(
    tracked_sequence: u32,
    tracked_attempts: u32,
    sequence: u32,
    max_attempts: u32,
) -> (u32, CaptureAttempt) {
    let attempts = if tracked_sequence == sequence {
        tracked_attempts.saturating_add(1)
    } else {
        // A different copy: this one starts its own budget.
        1
    };

    if attempts >= max_attempts {
        (attempts, CaptureAttempt::Handled)
    } else {
        (attempts, CaptureAttempt::Deferred)
    }
}

/// `Err(())` means the snapshot consumer is gone (process teardown).
#[cfg(target_os = "windows")]
fn capture_clipboard_update(
    mut sequence: u32,
    event_tx: &tokio::sync::mpsc::UnboundedSender<ClipboardListenerEvent>,
) -> Result<CaptureAttempt, ()> {
    let read_sequence =
        || unsafe { windows::Win32::System::DataExchange::GetClipboardSequenceNumber() };

    for bind_attempt in 0..MAX_SEQUENCE_BIND_ATTEMPTS {
        match capture_one_bound_sequence(sequence, &read_sequence, event_tx) {
            Ok(attempt) => return Ok(attempt),
            Err(BoundCaptureFailure::ConsumerGone) => return Err(()),
            Err(BoundCaptureFailure::Sequence(SequenceBindError::Changed { from, to })) => {
                log::debug!(
                    "CLIPBOARD: Sequence changed {from} -> {to} during capture (bind attempt {}); retrying from metadata",
                    bind_attempt + 1
                );
                // The clipboard now belongs to a newer copy. Bind the next
                // attempt to that sequence rather than the stale event number.
                sequence = to;
            }
        }
    }

    let current = read_sequence();
    note_clipboard_event(current);
    log::warn!(
        "CLIPBOARD: Discarded capture because sequence {} kept changing during read",
        current
    );
    Ok(CaptureAttempt::Handled)
}

#[cfg(target_os = "windows")]
enum BoundCaptureFailure {
    Sequence(SequenceBindError),
    ConsumerGone,
}

#[cfg(target_os = "windows")]
impl From<SequenceBindError> for BoundCaptureFailure {
    fn from(error: SequenceBindError) -> Self {
        Self::Sequence(error)
    }
}

/// One metadata-then-payload read, kept only if the sequence stays put.
#[cfg(target_os = "windows")]
fn capture_one_bound_sequence(
    mut sequence: u32,
    read_sequence: &impl Fn() -> u32,
    event_tx: &tokio::sync::mpsc::UnboundedSender<ClipboardListenerEvent>,
) -> Result<CaptureAttempt, BoundCaptureFailure> {
    let started = Instant::now();
    let live = read_sequence();
    if live != sequence {
        // The event sequence is already stale. Capture whatever is current.
        sequence = live;
    }

    // File clipboard payloads are references to external paths, not durable
    // clipboard content. Recording them as history creates entries that can
    // silently stop working after a move, disconnect, or target-app mismatch.
    // Ignore both physical and virtual file payloads before reading any text
    // fallback they may advertise. Screenshot tools are the exception: they
    // intentionally add CF_HDROP beside real image data, which Cubby retains
    // as an image rather than as an unreliable file reference.
    let has_file_payload = clipboard_has_file_payload_format();
    let has_image_payload = clipboard_has_image_format();
    let source_app_identity = get_clipboard_owner_identity();
    let sensitive = clipboard_marked_sensitive();
    let after_metadata = read_sequence();
    if after_metadata != sequence {
        return Err(SequenceBindError::Changed {
            from: sequence,
            to: after_metadata,
        }
        .into());
    }

    if crate::clipboard_policy::classify_file_payload(has_file_payload, has_image_payload)
        == crate::clipboard_policy::FilePayloadPolicy::IgnoreFilePayload
    {
        note_clipboard_event(sequence);
        log::debug!(
            "CLIPBOARD: Sequence {} contained a file payload; intentionally ignoring it",
            sequence
        );
        return Ok(CaptureAttempt::Handled);
    }

    let materialized = materialize_with_sequence_guard(
        sequence,
        read_sequence,
        |attempt| match materialize_clipboard_content_once(attempt) {
            MaterializeOnce::Captured(content, formats) => {
                Some(MaterializeAttempt::Captured(content, formats))
            }
            MaterializeOnce::DeterminateMiss => Some(MaterializeAttempt::DeterminateMiss),
            MaterializeOnce::Transient => {
                if attempt + 1 < 10 {
                    std::thread::sleep(clipboard_retry_delay(attempt));
                }
                None
            }
        },
        10,
    )?;

    persist_if_sequence_holds(sequence, read_sequence(), ())?;

    if let Some(MaterializeAttempt::Captured(content, formats)) = materialized {
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
            .map_err(|_| BoundCaptureFailure::ConsumerGone);
    }

    if matches!(materialized, Some(MaterializeAttempt::DeterminateMiss)) {
        // Opened the clipboard and read empty/missing text with no HTML/RTF
        // and no image. That is not a lock, even when CF_UNICODETEXT is still
        // advertised (SBS-924).
        if clipboard_is_cleared() {
            persist_if_sequence_holds(sequence, read_sequence(), ())?;
            note_clipboard_event(sequence);
            return event_tx
                .send(ClipboardListenerEvent::Cleared { sequence })
                .map(|_| CaptureAttempt::Handled)
                .map_err(|_| BoundCaptureFailure::ConsumerGone);
        }
        persist_if_sequence_holds(sequence, read_sequence(), ())?;
        note_clipboard_event(sequence);
        log::debug!(
            "CLIPBOARD: Sequence {} contained no supported text or image payload",
            sequence
        );
        return Ok(CaptureAttempt::Handled);
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
        persist_if_sequence_holds(sequence, read_sequence(), ())?;
        // Empty clipboard (or empty text with no image/files). This is
        // the password-manager auto-clear signal; never treat a new
        // non-empty copy as a clear.
        note_clipboard_event(sequence);
        return event_tx
            .send(ClipboardListenerEvent::Cleared { sequence })
            .map(|_| CaptureAttempt::Handled)
            .map_err(|_| BoundCaptureFailure::ConsumerGone);
    }

    if clipboard_has_supported_format() {
        persist_if_sequence_holds(sequence, read_sequence(), ())?;
        let (attempts, decision) = deferral_decision(
            DEFERRED_SEQUENCE.load(Ordering::SeqCst),
            DEFERRED_ATTEMPTS.load(Ordering::SeqCst),
            sequence,
            MAX_DEFERRALS_PER_SEQUENCE,
        );
        DEFERRED_SEQUENCE.store(sequence, Ordering::SeqCst);
        DEFERRED_ATTEMPTS.store(attempts, Ordering::SeqCst);

        if decision == CaptureAttempt::Deferred {
            log::warn!(
                "CLIPBOARD: Could not materialize sequence {} (clipboard contended); deferring for watchdog retry (attempt {} of {})",
                sequence,
                attempts,
                MAX_DEFERRALS_PER_SEQUENCE
            );
            return Ok(CaptureAttempt::Deferred);
        }

        // The owner never released the clipboard. Mark the sequence handled so
        // the watchdog stops restarting the listener over one lost copy, and
        // record it: the contract is that a failed capture is visible in
        // diagnostics rather than silently reported as a success.
        note_clipboard_event(sequence);
        record_capture_error(format!(
            "clipboard sequence {sequence} stayed locked across {attempts} attempts; that copy was not captured"
        ));
        return Ok(CaptureAttempt::Handled);
    }

    persist_if_sequence_holds(sequence, read_sequence(), ())?;
    // Nothing we support (custom/private formats only). Mark handled so the
    // watchdog does not treat an ignored format as a dead listener.
    note_clipboard_event(sequence);
    log::debug!(
        "CLIPBOARD: Sequence {} contained no supported text or image payload",
        sequence
    );
    Ok(CaptureAttempt::Handled)
}

/// True when the clipboard advertises a format `materialize_clipboard_content_once`
/// can read (text or an image). Used to tell "unsupported payload"
/// (mark handled) apart from "supported but contended" (defer and retry).
///
/// Advertised `CF_UNICODETEXT` is not enough after a determinate empty-text
/// read: that payload is empty, not locked (SBS-924). Callers that already
/// opened the clipboard and read empty text must not use this as a lock signal.
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
fn register_clipboard_format(name: &str) -> u32 {
    use windows::core::PCWSTR;
    use windows::Win32::System::DataExchange::RegisterClipboardFormatW;

    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe { RegisterClipboardFormatW(PCWSTR(wide.as_ptr())) }
}

#[cfg(target_os = "windows")]
fn registered_png_format() -> u32 {
    static PNG_FORMAT: OnceLock<u32> = OnceLock::new();
    *PNG_FORMAT.get_or_init(|| register_clipboard_format("PNG"))
}

#[cfg(target_os = "windows")]
fn clipboard_has_image_format() -> bool {
    use windows::Win32::System::DataExchange::IsClipboardFormatAvailable;

    if crate::clipboard_policy::PREDEFINED_IMAGE_FORMATS
        .into_iter()
        .any(|(_, format)| unsafe { IsClipboardFormatAvailable(format) }.is_ok())
    {
        return true;
    }

    registered_image_formats()
        .iter()
        .any(|format| *format != 0 && unsafe { IsClipboardFormatAvailable(*format) }.is_ok())
}

/// Identifiers for [`crate::clipboard_policy::REGISTERED_IMAGE_FORMATS`],
/// resolved once because this runs on the capture path.
#[cfg(target_os = "windows")]
fn registered_image_formats() -> &'static [u32] {
    static FORMATS: OnceLock<Vec<u32>> = OnceLock::new();
    FORMATS.get_or_init(|| {
        crate::clipboard_policy::REGISTERED_IMAGE_FORMATS
            .into_iter()
            .map(register_clipboard_format)
            .collect()
    })
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
            // content. Those copies are stored as rich clips (SBS-924).
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

/// One materialize attempt. `attempt` is 0-based so the last try can do a
/// slower image decode. The sequence-bound capture path checks the clipboard
/// sequence around each call instead of sleeping across a sequence change.
///
/// Empty Unicode text is a determinate miss, not a retryable lock: if HTML or
/// RTF has content we store that clip; otherwise the caller marks the sequence
/// handled (or cleared) without restarting the listener (SBS-924).
fn materialize_clipboard_content_once(attempt: u32) -> MaterializeOnce {
    const ATTEMPTS: u32 = 10;
    let last_attempt = attempt + 1 == ATTEMPTS;

    // Screenshot tools commonly expose both a bitmap and CF_HDROP for the
    // saved image. Treat that as an image in Cubby. If the advertised image
    // is still being rendered, do not let the easier file read mask it
    // immediately; retry the image for the complete bounded window.
    if clipboard_has_image_format() && clipboard_has_file_payload_format() {
        if let Ok(image) = read_clipboard_image_fast(last_attempt) {
            return MaterializeOnce::Captured(captured_image(image), Vec::new());
        }
        // The caller records this hybrid update as handled on the last miss.
        // Do not fall through to text, where the path could be captured as a
        // text clip.
        return MaterializeOnce::Transient;
    }

    if let Ok(ctx) = ClipboardContext::new() {
        let text = ctx.get_text().ok();
        let html = ctx.get_html().ok();
        let rtf = ctx.get_rich_text().ok();
        let text_read = observed_payload(&text, clipboard_has_unicode_text_format());
        let html_read = observed_payload(&html, clipboard_has_html_format());
        let rtf_read = observed_payload(&rtf, clipboard_has_rtf_format());

        // Only decode when the Unicode body cannot carry the copy on its own.
        // A non-empty text clip never needs the bitmap, and decoding one on
        // every attempt would pay for pixels nobody stores.
        let image = if matches!(
            text_read,
            crate::clipboard_miss::PayloadRead::Present(body) if !body.is_empty()
        ) {
            None
        } else {
            read_clipboard_image_fast(last_attempt).ok()
        };

        let decision = crate::clipboard_miss::decide_capture(crate::clipboard_miss::AttemptFacts {
            text: text_read,
            html: html_read,
            rtf: rtf_read,
            image_advertised: clipboard_has_image_format(),
            image_readable: image.is_some(),
            last_attempt,
        });
        return match decision {
            crate::clipboard_miss::CaptureDecision::Image => match image {
                Some(image) => MaterializeOnce::Captured(captured_image(image), Vec::new()),
                None => MaterializeOnce::Transient,
            },
            crate::clipboard_miss::CaptureDecision::Rich(rich) => {
                match capture_text(rich.searchable_text) {
                    Some(content) => MaterializeOnce::Captured(
                        content,
                        captured_rich_formats(rich.html, rich.rtf),
                    ),
                    // The body strips to nothing after all. Nothing here will
                    // change on a retry, so do not restart the listener.
                    None => MaterializeOnce::DeterminateMiss,
                }
            }
            crate::clipboard_miss::CaptureDecision::DeterminateMiss => {
                MaterializeOnce::DeterminateMiss
            }
            crate::clipboard_miss::CaptureDecision::Transient => MaterializeOnce::Transient,
        };
    }

    if let Ok(image) = read_clipboard_image_fast(last_attempt) {
        return MaterializeOnce::Captured(captured_image(image), Vec::new());
    }
    MaterializeOnce::Transient
}

fn observed_payload(
    value: &Option<String>,
    advertised: bool,
) -> crate::clipboard_miss::PayloadRead<'_> {
    match value {
        Some(body) => crate::clipboard_miss::PayloadRead::Present(body),
        None if advertised => crate::clipboard_miss::PayloadRead::Unknown,
        None => crate::clipboard_miss::PayloadRead::Missing,
    }
}

#[cfg(target_os = "windows")]
fn clipboard_has_unicode_text_format() -> bool {
    const CF_UNICODETEXT: u32 = 13;
    unsafe { windows::Win32::System::DataExchange::IsClipboardFormatAvailable(CF_UNICODETEXT) }
        .is_ok()
}

#[cfg(not(target_os = "windows"))]
fn clipboard_has_unicode_text_format() -> bool {
    false
}

#[cfg(target_os = "windows")]
fn clipboard_has_html_format() -> bool {
    let format = register_clipboard_format("HTML Format");
    format != 0
        && unsafe { windows::Win32::System::DataExchange::IsClipboardFormatAvailable(format) }
            .is_ok()
}

#[cfg(not(target_os = "windows"))]
fn clipboard_has_html_format() -> bool {
    false
}

#[cfg(target_os = "windows")]
fn clipboard_has_rtf_format() -> bool {
    let format = register_clipboard_format("Rich Text Format");
    format != 0
        && unsafe { windows::Win32::System::DataExchange::IsClipboardFormatAvailable(format) }
            .is_ok()
}

#[cfg(not(target_os = "windows"))]
fn clipboard_has_rtf_format() -> bool {
    false
}

fn captured_rich_formats(html: Option<String>, rtf: Option<String>) -> Vec<CapturedFormat> {
    let mut formats = Vec::new();
    if let Some(html) = html {
        formats.push(CapturedFormat {
            name: "html",
            content: html.into_bytes(),
        });
    }
    if let Some(rtf) = rtf {
        formats.push(CapturedFormat {
            name: "rtf",
            content: rtf.into_bytes(),
        });
    }
    formats
}

/// Outcome of one materialize attempt. Transient misses retry; a determinate
/// miss (opened, empty/missing text, no HTML/RTF, no image) must not.
enum MaterializeOnce {
    Captured(CapturedContent, Vec<CapturedFormat>),
    DeterminateMiss,
    Transient,
}

/// Stop retrying once we know the copy is captured or is a determinate miss.
enum MaterializeAttempt {
    Captured(CapturedContent, Vec<CapturedFormat>),
    DeterminateMiss,
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
    use windows::Win32::System::DataExchange::IsClipboardFormatAvailable;

    let format = registered_png_format();
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
        // Decode before opening: a failure here must leave the existing
        // clipboard untouched rather than emptied.
        let prepared = prepare_image_clipboard_formats(png_bytes)?;
        let _clipboard = clipboard_win::Clipboard::new_attempts(10)
            .map_err(|e| format!("could not open clipboard: {e}"))?;
        clipboard_win::raw::empty().map_err(|e| format!("could not clear clipboard: {e}"))?;
        write_image_clipboard_formats(&prepared)
    }

    #[cfg(not(target_os = "windows"))]
    {
        let image = RustImageData::from_bytes(png_bytes).map_err(|e| e.to_string())?;
        ClipboardContext::new()
            .and_then(|context| context.set_image(image))
            .map_err(|e| e.to_string())
    }
}

/// The PNG and CF_DIB renderings of one image, plus the registered PNG format,
/// all resolved *before* the clipboard is opened.
///
/// Everything that can fail belongs in here. Decoding after the clipboard has
/// been emptied means an undecodable PNG — or a `rgba_to_cf_dib` guard — leaves
/// the user with an empty clipboard and nothing written, which also trips
/// `forget_on_clipboard_clear`. Decoding a multi-megapixel screenshot also takes
/// long enough that holding the global clipboard handle across it stalls every
/// other application that wants to read or write.
#[cfg(target_os = "windows")]
struct PreparedImageFormats<'a> {
    png_format: u32,
    png: &'a [u8],
    dib: Vec<u8>,
}

#[cfg(target_os = "windows")]
fn prepare_image_clipboard_formats(png_bytes: &[u8]) -> Result<PreparedImageFormats<'_>, String> {
    let rgba = image::load_from_memory(png_bytes)
        .map_err(|e| e.to_string())?
        .into_rgba8();
    let dib = rgba_to_cf_dib(rgba.width(), rgba.height(), rgba.as_raw())?;
    let png_format = registered_png_format();
    if png_format == 0 {
        return Err("could not register the PNG clipboard format".to_string());
    }
    Ok(PreparedImageFormats {
        png_format,
        png: png_bytes,
        dib,
    })
}

/// Write prepared image formats onto an already-open, already-emptied clipboard,
/// so callers can publish additional formats under the same handle.
///
/// Both formats are required: PNG preserves byte-stable image identity for
/// self-write suppression, while CF_DIB supports traditional Windows paste
/// targets. If the second write fails, clear the partial clipboard rather than
/// reporting success with an inconsistent payload.
#[cfg(target_os = "windows")]
fn write_image_clipboard_formats(prepared: &PreparedImageFormats<'_>) -> Result<(), String> {
    clipboard_win::raw::set_without_clear(prepared.png_format, prepared.png)
        .map_err(|error| format!("could not set PNG: {error}"))?;
    if let Err(dib_error) = clipboard_win::raw::set_without_clear(CF_DIB_FORMAT, &prepared.dib) {
        return match clipboard_win::raw::empty() {
            Ok(()) => Err(format!("could not set CF_DIB: {dib_error}")),
            Err(cleanup_error) => Err(format!(
                "could not set CF_DIB ({dib_error}) or clear partial PNG ({cleanup_error})"
            )),
        };
    }
    Ok(())
}

/// True when the clipboard owner is a recognized remote-control client rather
/// than an ordinary local application.
///
/// This distinction carries weight beyond paste strategy. A remote client
/// writes whatever it received from the far end, so the metadata on that write
/// describes the *viewer's* policy, not what the originating application meant.
fn is_remote_client_process(exe_name: Option<&str>) -> bool {
    exe_name.is_some_and(|exe| {
        crate::paste_engine::paste_strategy_for_process(exe)
            != crate::paste_engine::PasteStrategy::Standard
    })
}

/// True when a remote client is the *attributed owner* of this clipboard write.
///
/// `is_explicit_owner` is load-bearing, not a nicety. When `GetClipboardOwner`
/// fails, attribution falls back to the foreground window — so a local password
/// manager that writes without claiming ownership, while a remote client
/// happens to be focused, would otherwise be classified as remote. That would
/// relay its secret into every remote session and bypass `skip_sensitive` on
/// the way into history. An unattributed write is never treated as remote.
fn is_remote_client_owner(exe_name: Option<&str>, is_explicit_owner: bool) -> bool {
    is_explicit_owner && is_remote_client_process(exe_name)
}

/// Relay only captures owned by a remote-control viewer.
///
/// Sensitive-tagged content is relayed too, because a remote client tags
/// everything it syncs down (SBS-781). [`relay_remote_capture`] re-applies the
/// marker so the rewrite does not weaken it for other monitors.
///
/// An ignored application is excluded from transport as well as from history.
/// Naming an app in `ignored_apps` means keep Cubby out of it, not merely keep
/// it out of the list, and it is the only way to disable the relay for one
/// viewer while keeping it for another. Storage *heuristics* deliberately do not
/// gate relaying: a misfiring classifier there would strand a copy in one
/// session with no visible reason.
fn should_relay_capture(relay_enabled: bool, remote_client: bool, app_ignored: bool) -> bool {
    relay_enabled && remote_client && !app_ignored
}

/// Whether a sensitive-tagged capture should be dropped instead of stored.
///
/// A remote client tags everything it forwards, because it cannot know what the
/// far-end application meant. Honoring that dropped every remote-session copy
/// and left a hole in history that reads as lost data (SBS-781), so a
/// recognized remote client's marker does not suppress storage. A local
/// application's marker still does: it is set deliberately, about content that
/// application owns.
fn should_skip_sensitive_capture(
    skip_sensitive: bool,
    sensitive: bool,
    remote_client: bool,
) -> bool {
    skip_sensitive && sensitive && !remote_client
}

/// Outcome of the skip-likely-secrets capture gate.
///
/// `Unscannable` is its own state: failing to decode the paste is not the
/// same as "not a secret", so capture refuses to store it (SBS-922).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LikelySecretDecision {
    Store,
    Skip(crate::secrets::SecretKind),
    Unscannable,
}

fn likely_secret_decision(
    skip_likely_secrets: bool,
    clip_type: &str,
    clip_content: &[u8],
) -> LikelySecretDecision {
    if !skip_likely_secrets || clip_type != "text" {
        return LikelySecretDecision::Store;
    }
    match std::str::from_utf8(clip_content) {
        Ok(text) => match crate::secrets::classify_secret(text) {
            Some(kind) => LikelySecretDecision::Skip(kind),
            None => LikelySecretDecision::Store,
        },
        Err(_) => LikelySecretDecision::Unscannable,
    }
}

/// `CF_UNICODETEXT` payload bytes: UTF-16LE, NUL-terminated as Win32 requires.
#[cfg(target_os = "windows")]
fn utf16_clipboard_bytes(text: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity((text.len() + 1) * 2);
    for unit in text.encode_utf16().chain(std::iter::once(0)) {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

/// Payload written under the sensitive marker format.
///
/// Deliberately not empty. `clipboard_win::raw::set_inner` returns `Ok(())`
/// without ever calling `SetClipboardData` when the slice is zero-length, so an
/// empty marker reports success while writing nothing at all. Monitors only
/// test presence through `IsClipboardFormatAvailable`, so one byte is enough.
#[cfg(target_os = "windows")]
const SENSITIVE_MARKER_PAYLOAD: &[u8] = &[0];

#[cfg(target_os = "windows")]
fn registered_sensitive_format() -> u32 {
    static SENSITIVE_FORMAT: OnceLock<u32> = OnceLock::new();
    *SENSITIVE_FORMAT
        .get_or_init(|| register_clipboard_format("ExcludeClipboardContentFromMonitorProcessing"))
}

/// Publish a relayed payload, and its sensitive marker, in one clipboard open.
///
/// Atomicity is the point. Publishing the content and then reopening to add
/// `ExcludeClipboardContentFromMonitorProcessing` leaves a window where another
/// monitor reads the content before the marker exists, and where a clipboard
/// change between the two writes would tag *someone else's* content as
/// sensitive. Both formats therefore go out under a single clipboard handle,
/// before any other process can observe the update.
#[cfg(target_os = "windows")]
fn set_relayed_clipboard_payload(
    clip_content: &[u8],
    image_content: Option<&[u8]>,
    captured_formats: &[CapturedFormat],
    sensitive: bool,
) -> Result<(), String> {
    const CF_UNICODETEXT_FORMAT: u32 = 13;

    // Everything that can fail happens before the clipboard is opened, so a
    // failure leaves the user's existing clipboard intact instead of emptied,
    // and the global handle is never held across an image decode.
    let prepared_image = image_content
        .map(prepare_image_clipboard_formats)
        .transpose()?;
    let sensitive_marker = if sensitive {
        let marker = registered_sensitive_format();
        if marker == 0 {
            return Err("could not register the sensitive marker format".to_string());
        }
        Some(marker)
    } else {
        None
    };

    let _clipboard = clipboard_win::Clipboard::new_attempts(10)
        .map_err(|error| format!("could not open clipboard: {error}"))?;
    clipboard_win::raw::empty().map_err(|error| format!("could not clear clipboard: {error}"))?;

    if let Some(prepared) = prepared_image.as_ref() {
        write_image_clipboard_formats(prepared)?;
    } else {
        let text = String::from_utf8_lossy(clip_content);
        clipboard_win::raw::set_without_clear(CF_UNICODETEXT_FORMAT, &utf16_clipboard_bytes(&text))
            .map_err(|error| format!("could not set text: {error}"))?;

        for (name, document) in relayed_auxiliary_formats(captured_formats) {
            // Capture stores the StartHTML..EndHTML document with the header
            // stripped, so the header has to be rebuilt here exactly as the
            // restore path does. Writing the bare document produces an "HTML
            // Format" entry Office-class apps reject.
            let (clipboard_format, payload) = match name {
                "html" => (
                    register_clipboard_format("HTML Format"),
                    crate::cf_html::to_cf_html(&String::from_utf8_lossy(&document)).into_bytes(),
                ),
                "rtf" => (register_clipboard_format("Rich Text Format"), document),
                _ => continue,
            };
            if clipboard_format == 0 {
                continue;
            }
            // An auxiliary format is a fidelity bonus, not the payload. Losing
            // one must not cost the user the relay itself.
            if let Err(error) = clipboard_win::raw::set_without_clear(clipboard_format, &payload) {
                log::warn!("CLIPBOARD: Could not relay the {name} format: {error}");
            }
        }
    }

    if let Some(marker) = sensitive_marker {
        // Failing here is fatal to the write, and returning early is not enough:
        // the payload is already on the clipboard, so bailing out would publish
        // tagged content without its tag — exactly what this function exists to
        // prevent. Empty the clipboard so the secret is withdrawn with it.
        if let Err(error) = clipboard_win::raw::set_without_clear(marker, SENSITIVE_MARKER_PAYLOAD)
        {
            return match clipboard_win::raw::empty() {
                Ok(()) => Err(format!("could not re-apply the sensitive marker: {error}")),
                Err(cleanup_error) => Err(format!(
                    "could not re-apply the sensitive marker ({error}); \
                     the untagged payload could not be withdrawn either ({cleanup_error})"
                )),
            };
        }
    }
    Ok(())
}

/// The auxiliary formats a relay republishes, in the form a re-capture reads
/// them back: `get_html` returns the StartHTML..EndHTML slice, so HTML is
/// normalized here through [`crate::cf_html::document`] and the write side
/// re-attaches the CF_HTML header on top of it.
///
/// One source of truth on purpose. The write publishes these payloads and
/// [`relayed_clip_hash`] predicts the re-capture from them; a format added to
/// only one of the two sites would silently stop the predicted hash matching,
/// and Cubby would store its own relay as a duplicate.
fn relayed_auxiliary_formats(captured_formats: &[CapturedFormat]) -> Vec<(&'static str, Vec<u8>)> {
    captured_formats
        .iter()
        .filter_map(|format| match format.name {
            "html" => Some((
                "html",
                crate::cf_html::document(&String::from_utf8_lossy(&format.content)).into_bytes(),
            )),
            "rtf" => Some(("rtf", format.content.clone())),
            _ => None,
        })
        .collect()
}

/// The hash a re-capture of our own relay write will produce.
///
/// The relay does not republish the capture byte-for-byte, so hashing the
/// captured bytes arms the ignore marker with a hash that never arrives and
/// Cubby stores its own relay as a duplicate clip attributed to itself. Two
/// divergences: HTML goes out through [`crate::cf_html::to_cf_html`], so a
/// re-capture reads back the normalized `document` slice rather than the
/// original bytes; and formats other than html and rtf are not written at all.
/// Images are unaffected — the same PNG bytes go back out, and format material
/// is excluded from image hashes.
fn relayed_clip_hash(
    clip_type: &str,
    clip_content: &[u8],
    full_image_content: Option<&[u8]>,
    captured_formats: &[CapturedFormat],
) -> String {
    let relayed_formats = relayed_auxiliary_formats(captured_formats);
    let material = build_clip_hash_material(
        clip_type,
        full_image_content.unwrap_or(clip_content),
        relayed_formats
            .iter()
            .map(|(name, content)| (*name, content.as_slice())),
    );
    calculate_hash(&material)
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
    sensitive: bool,
) {
    let image_content = if clip_type == "image" {
        let Some(png_bytes) = full_image_content else {
            return;
        };
        Some(png_bytes)
    } else {
        None
    };
    {
        let mut last = LAST_RELAYED_HASH.lock();
        if last.as_deref() == Some(clip_hash) {
            return;
        }
        *last = Some(clip_hash.to_string());
    }
    // The marker has to describe our *write*, not the capture that prompted it.
    let relayed_hash = relayed_clip_hash(
        clip_type,
        clip_content,
        full_image_content,
        captured_formats,
    );
    set_ignore_hash(relayed_hash.clone());

    #[cfg(target_os = "windows")]
    let set_result =
        set_relayed_clipboard_payload(clip_content, image_content, captured_formats, sensitive);

    // Only Windows has the sensitive marker, and only Windows recognizes a
    // remote client in the first place, so the portable path stays as it was.
    #[cfg(not(target_os = "windows"))]
    let set_result = {
        let _ = sensitive;
        if let Some(png_bytes) = image_content {
            set_clipboard_image_png(png_bytes)
        } else {
            let mut contents = vec![ClipboardContent::Text(
                String::from_utf8_lossy(clip_content).to_string(),
            )];
            for format in captured_formats {
                match format.name {
                    "html" => contents.push(ClipboardContent::Html(crate::cf_html::to_cf_html(
                        &String::from_utf8_lossy(&format.content),
                    ))),
                    "rtf" => contents.push(ClipboardContent::Rtf(
                        String::from_utf8_lossy(&format.content).to_string(),
                    )),
                    _ => {}
                }
            }
            ClipboardContext::new()
                .and_then(|context| context.set(contents))
                .map_err(|error| error.to_string())
        }
    };

    match set_result {
        Ok(()) => {
            if sensitive {
                log::info!(
                    "CLIPBOARD: Relayed remote-session capture under Cubby ownership, sensitive marker preserved"
                );
            } else {
                log::info!("CLIPBOARD: Relayed remote-session capture under Cubby ownership");
            }
        }
        Err(error) => {
            clear_ignore_hash_if_matches(&relayed_hash);
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
        if ignore_marker_applies(
            lock.as_ref(),
            clip_hash.as_str(),
            Instant::now(),
            IGNORE_HASH_TTL,
        ) {
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

    let remote_client = is_remote_client_owner(exe_name.as_deref(), is_explicit_owner);

    // Resolved before the relay because an ignored application is excluded from
    // transport too, not just from history.
    let is_ignored = |name: &str| {
        let name_lower = name.to_lowercase();
        settings
            .ignored_apps
            .iter()
            .any(|app| app.to_lowercase() == name_lower)
    };
    let ignored_match = full_path
        .as_deref()
        .filter(|path| is_ignored(path))
        .map(|_| "path")
        .or_else(|| {
            exe_name
                .as_deref()
                .filter(|exe| is_ignored(exe))
                .map(|_| "executable")
        });

    // Re-announce a remote copy before the rest of capture policy runs. Relaying
    // is clipboard transport; storing is history. Gating transport behind
    // storage rules is what let a privacy preference silently break copy-
    // between-sessions, and put the relay behind the whole encrypt-and-write
    // path so a viewer could sample a stale clipboard first (SBS-781).
    if should_relay_capture(
        settings.remote_clipboard_relay,
        remote_client,
        ignored_match.is_some(),
    ) {
        relay_remote_capture(
            clip_type,
            &clip_content,
            full_image_content.as_deref(),
            &captured_formats,
            &clip_hash,
            sensitive,
        );
    }

    if should_skip_sensitive_capture(settings.skip_sensitive, sensitive, remote_client) {
        log::info!("CLIPBOARD: Skipping content the source app marked as sensitive");
        discard_clear_target();
        return;
    }

    match likely_secret_decision(settings.skip_likely_secrets, clip_type, &clip_content) {
        LikelySecretDecision::Store => {}
        LikelySecretDecision::Skip(kind) => {
            // Category only — never log the matched clipboard bytes.
            log::info!("CLIPBOARD: Skipping likely secret ({})", kind.as_str());
            discard_clear_target();
            return;
        }
        LikelySecretDecision::Unscannable => {
            log::info!("CLIPBOARD: Skipping text that could not be scanned for secrets");
            discard_clear_target();
            return;
        }
    }

    if settings.ignore_ghost_clips && !is_explicit_owner {
        log::info!("CLIPBOARD: Ignoring ghost clip (unknown owner)");
        discard_clear_target();
        return;
    }

    if let Some(matched_on) = ignored_match {
        log::info!("CLIPBOARD: Ignoring content from configured application ({matched_on} match)");
        discard_clear_target();
        return;
    }

    // Only accepted content participates in consecutive duplicate suppression.
    // An ignored application must not prevent the same content from being captured later.
    //
    // Hash equality alone is not enough for images: retention can expire the
    // stored original while LAST_STABLE_HASH still holds this hash, and
    // skipping here would never revive it (SBS-769). Look up the stored row
    // when the hashes match. Text keeps today's skip.
    let storage_hash = db.crypto.keyed_hash(&hash_material);
    {
        let consecutive_match = LAST_STABLE_HASH.lock().as_ref() == Some(&clip_hash);
        if consecutive_match {
            if clip_type != "image" {
                return;
            }
            let stored = lookup_stored_original(&db.pool, &storage_hash).await;
            if consecutive_dedup_decision(stored) == ConsecutiveDedupDecision::Skip {
                return;
            }
        }
    }

    // Past this point every early return is a failed capture, and a failure must
    // not leave the *previous* clip as the clear-forget target: copy password A,
    // fail to store password B, and the manager's auto-clear would then delete A.
    // Clearing here rather than at each error return means new failure paths get
    // this for free. Success re-establishes the marker at the end of the
    // function, so the only cost is that a failure forgets a clear target that
    // was about to be replaced anyway.
    discard_clear_target();

    // DB Logic
    let pool = &db.pool;
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
    // A failed lookup must not be read as "no such row". content_hash is unique
    // as of the idx_clips_hash_unique migration, so the insert below would now
    // be rejected rather than silently storing a second row -- but a rejected
    // insert loses the capture just the same, and reports it as a constraint
    // error rather than as what it is. Since we cannot tell whether the clip
    // already exists, skip this capture and say so: one recorded miss beats
    // either a duplicate or a confusing failure further down.
    let existing_uuid: Option<String> = match sqlx::query_scalar::<_, String>(
        r#"SELECT uuid FROM clips WHERE content_hash = ?"#,
    )
    .bind(&storage_hash)
    .fetch_optional(pool)
    .await
    {
        Ok(found) => found,
        Err(error) => {
            // record_capture_error both logs and stores, so the whole reason
            // belongs in one message: last_error is what
            // get_clipboard_capture_status surfaces, and "lookup failed" on its
            // own does not explain why the copy was dropped.
            record_capture_error(format!(
                "could not check whether sequence {sequence} was already stored, so it was skipped to avoid a duplicate row: {error}"
            ));
            return;
        }
    };
    let db_lookup_ms = db_lookup_started.elapsed().as_millis();

    let db_write_started = std::time::Instant::now();
    let (emitted_id, inserted_new) = if let Some(existing_id) = existing_uuid {
        was_existing = true;
        if clip_type == "image" {
            if let Some(full_bytes) = &full_image_content {
                // Stage the new original to a temp file, commit the row change,
                // and only then move it over the previous `{uuid}.cubby`. A
                // failure at any step is a capture miss: keep the prior row and
                // the prior original, and do not enqueue OCR.
                if recapture_existing_image(
                    &db,
                    &existing_id,
                    full_bytes,
                    crate::image_persist::RecaptureFields {
                        source_app: &encrypted_source_app,
                        source_icon: &encrypted_source_icon,
                        content: &encrypted_content,
                        preview: &encrypted_preview,
                        metadata: encrypted_metadata.clone(),
                    },
                )
                .await
                .is_err()
                {
                    return;
                }
            } else if let Err(error) = sqlx::query(
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
            // No second pass over the same file. `recapture_existing_image`
            // above already staged the original, upserted `clip_images`, and
            // cleared `full_image_expired` in one transaction. Writing
            // `{uuid}.cubby` again here was not just wasted work: it was the
            // only place the expired flag was cleared, so a failure of that
            // second write (no room for another temp copy, or Windows refusing
            // to replace the file recapture had just created) left the original
            // on disk and indexed while Paste and Copy went on refusing it as
            // expired (SBS-769).
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
            let image_path: Option<String> = match sqlx::query_scalar(
                "SELECT file_path FROM clip_images WHERE clip_uuid = ?",
            )
            .bind(&emitted_id)
            .fetch_optional(pool)
            .await
            {
                Ok(path) => path,
                Err(error) => {
                    // A failed lookup is not "no row". Cascading the clip
                    // delete would drop the path before we could collect it,
                    // so fall back to the managed `{uuid}.cubby` name.
                    log::warn!(
                            "CLIPBOARD: Failed to look up image path while rolling back {emitted_id}: {error}"
                        );
                    Some(
                        db.image_dir
                            .join(format!("{emitted_id}.cubby"))
                            .to_string_lossy()
                            .to_string(),
                    )
                }
            };
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
        db.search_index.upsert(
            &emitted_id,
            clip_type,
            &clip_content,
            &clip_preview,
            None,
            source_app.as_deref(),
        );
    }

    // Remember this capture so a short-lived auto-clear can forget it (SOU-316).
    let credential_like = clip_type == "text"
        && std::str::from_utf8(&clip_content).is_ok_and(crate::secrets::looks_like_credential);
    *LAST_ACCEPTED_CAPTURE.lock() = Some(RecentCapture {
        uuid: emitted_id.clone(),
        captured_at: Instant::now(),
        credential_like,
    });

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

    // A failed SELECT must not be read as "already gone". unwrap_or(None)
    // dropped the retry marker on a transient lock, so a later clear could
    // not forget the password that was still in history (SBS-831).
    let uuid = recent.uuid.clone();
    let lookup = ForgetClipLookup::from_query(
        sqlx::query_as::<_, (i64, i64)>(
            r#"
            SELECT is_pinned, is_deleted
            FROM clips
            WHERE uuid = ?
            "#,
        )
        .bind(&uuid)
        .fetch_optional(pool)
        .await,
        recent,
    );

    let (is_pinned, is_deleted, recent) = match lookup {
        ForgetClipLookup::Found {
            row: (is_pinned, is_deleted),
            taken,
        } => (is_pinned, is_deleted, taken),
        ForgetClipLookup::AlreadyGone => {
            log::debug!(
                "CLIPBOARD: Clear sequence {} — recent capture {} already gone",
                sequence,
                uuid
            );
            return;
        }
        ForgetClipLookup::Failed { error, taken } => {
            log::error!(
                "CLIPBOARD: forget-on-clear skipped for sequence {sequence} (clip {}); lookup failed, so the retry marker was restored: {error}",
                taken.uuid
            );
            restore_marker(taken);
            return;
        }
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
        match sqlx::query_scalar(r#"SELECT file_path FROM clip_images WHERE clip_uuid = ?"#)
            .bind(&recent.uuid)
            .fetch_optional(&mut *transaction)
            .await
        {
            Ok(path) => path,
            Err(error) => {
                log::warn!(
                    "CLIPBOARD: Failed to look up image path while forgetting {}: {error}",
                    recent.uuid
                );
                Some(
                    db.image_dir
                        .join(format!("{}.cubby", recent.uuid))
                        .to_string_lossy()
                        .to_string(),
                )
            }
        };

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

pub use crate::image_persist::persist_full_image_file;

/// Stage a recaptured full-resolution original, update the existing image row
/// inside one transaction, and replace the previous original only after that
/// transaction commits. Any failure leaves the prior row and the prior file
/// intact and never queues OCR against a file that was never written.
async fn recapture_existing_image(
    db: &Database,
    existing_id: &str,
    full_bytes: &[u8],
    fields: crate::image_persist::RecaptureFields<'_>,
) -> Result<(), String> {
    match crate::image_persist::apply_existing_image_recapture(
        &db.pool,
        existing_id,
        crate::image_persist::stage_full_image_file(
            &db.crypto,
            &db.image_dir,
            existing_id,
            full_bytes,
        ),
        full_bytes.len() as i64,
        fields,
    )
    .await
    {
        Ok(()) => Ok(()),
        Err(error) => {
            record_capture_error(error.clone());
            Err(error)
        }
    }
}

/// How a consecutive-hash match should be treated once we have looked up the
/// stored row (SBS-769).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConsecutiveDedupDecision {
    Skip,
    Capture,
}

/// Result of looking up the stored clip for a consecutive-hash match.
///
/// Three states, not two: a failed lookup is not "no row".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StoredOriginalLookup {
    /// No clip with this hash is stored. Today's skip still applies.
    Missing,
    /// Row exists; `true` means retention expired the original.
    Found { full_image_expired: bool },
    /// The database could not be read. Treat as unknown: do not skip.
    Unknown,
}

/// Decide whether a consecutive-hash match should skip capture.
///
/// Today's helper skipped whenever the hashes matched. That drops revival
/// after retention expires an original: `LAST_STABLE_HASH` still holds the
/// hash, so the re-copy never reaches the persist path.
pub(crate) fn consecutive_dedup_decision(stored: StoredOriginalLookup) -> ConsecutiveDedupDecision {
    match stored {
        StoredOriginalLookup::Missing => ConsecutiveDedupDecision::Skip,
        StoredOriginalLookup::Found {
            full_image_expired: false,
        } => ConsecutiveDedupDecision::Skip,
        StoredOriginalLookup::Found {
            full_image_expired: true,
        }
        | StoredOriginalLookup::Unknown => ConsecutiveDedupDecision::Capture,
    }
}

pub(crate) async fn lookup_stored_original(
    pool: &sqlx::SqlitePool,
    storage_hash: &str,
) -> StoredOriginalLookup {
    match sqlx::query_scalar::<_, bool>(
        "SELECT full_image_expired FROM clips WHERE content_hash = ?",
    )
    .bind(storage_hash)
    .fetch_optional(pool)
    .await
    {
        Ok(None) => StoredOriginalLookup::Missing,
        Ok(Some(full_image_expired)) => StoredOriginalLookup::Found { full_image_expired },
        Err(error) => {
            log::warn!(
                "CLIPBOARD: could not look up stored original for consecutive-dedup: {error}"
            );
            StoredOriginalLookup::Unknown
        }
    }
}

/// Why restoring an existing image original failed.
///
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
        // OpenProcess returns a kernel handle. Every other capture path
        // (paste target, Win+V helper) CloseHandle's it; this one did not, so
        // each captured clip leaked a process handle until Cubby exited (SBS-1004).
        struct ProcessHandleGuard(windows::Win32::Foundation::HANDLE);
        impl Drop for ProcessHandleGuard {
            fn drop(&mut self) {
                unsafe {
                    let _ = CloseHandle(self.0);
                }
            }
        }
        let _process_guard = ProcessHandleGuard(process_handle);

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
        clear_ignore_hash_if_matches, clipboard_retry_delay, ignore_marker_applies,
        is_remote_client_owner, is_remote_client_process, likely_secret_decision,
        next_listener_backoff, relayed_clip_hash, rgba_to_cf_dib, set_ignore_hash,
        should_forget_recent_capture, should_relay_capture, should_skip_sensitive_capture,
        CapturedContent, CapturedFormat, LikelySecretDecision, CAPTURE_STATE_LISTENING,
        CAPTURE_STATE_RESTARTING, CAPTURE_STATE_STOPPED, CLIPBOARD_CLEAR_FORGET_WINDOW,
        IGNORE_HASH, IGNORE_HASH_TTL,
    };
    use std::time::{Duration, Instant};

    #[cfg(target_os = "windows")]
    #[test]
    fn cf_dib_format_matches_the_shared_image_format_table() {
        let table_dib = crate::clipboard_policy::PREDEFINED_IMAGE_FORMATS
            .into_iter()
            .find(|(name, _)| *name == "CF_DIB")
            .expect("CF_DIB in the shared image format table")
            .1;
        assert_eq!(super::CF_DIB_FORMAT, table_dib);
    }

    #[cfg(target_os = "windows")]
    mod deferral {
        use super::super::{deferral_decision, CaptureAttempt, MAX_DEFERRALS_PER_SEQUENCE};

        #[test]
        fn retries_a_contended_sequence_until_the_budget_runs_out() {
            // First contention on sequence 42, nothing tracked yet.
            assert_eq!(
                deferral_decision(0, 0, 42, 3),
                (1, CaptureAttempt::Deferred)
            );
            assert_eq!(
                deferral_decision(42, 1, 42, 3),
                (2, CaptureAttempt::Deferred)
            );
            // Third attempt exhausts the budget: write the copy off instead of
            // leaving the watchdog to restart the listener forever.
            assert_eq!(
                deferral_decision(42, 2, 42, 3),
                (3, CaptureAttempt::Handled)
            );
        }

        #[test]
        fn a_new_sequence_starts_its_own_budget() {
            // The previous copy burned every attempt; a fresh copy must not
            // inherit that and be dropped on its first contended read.
            assert_eq!(
                deferral_decision(42, 3, 43, 3),
                (1, CaptureAttempt::Deferred)
            );
        }

        #[test]
        fn saturates_instead_of_overflowing() {
            assert_eq!(
                deferral_decision(42, u32::MAX, 42, 3),
                (u32::MAX, CaptureAttempt::Handled)
            );
        }

        #[test]
        fn the_shipped_budget_allows_more_than_one_try() {
            // A budget of 1 would give up on the first contended read, which
            // defeats the deferral mechanism entirely.
            assert_eq!(
                deferral_decision(0, 0, 42, MAX_DEFERRALS_PER_SEQUENCE),
                (1, CaptureAttempt::Deferred)
            );
        }
    }

    #[test]
    fn relays_only_remote_viewer_captures() {
        assert!(should_relay_capture(
            true,
            is_remote_client_owner(Some("ncplayer.exe"), true),
            false
        ));
        assert!(should_relay_capture(
            true,
            is_remote_client_owner(Some("mstsc.exe"), true),
            false
        ));
        assert!(!should_relay_capture(
            true,
            is_remote_client_owner(Some("notepad.exe"), true),
            false
        ));
        assert!(!should_relay_capture(
            true,
            is_remote_client_owner(None, true),
            false
        ));
    }

    #[test]
    fn relay_respects_setting() {
        assert!(!should_relay_capture(
            false,
            is_remote_client_owner(Some("ncplayer.exe"), true),
            false
        ));
    }

    mod sequence_bind {
        use super::super::{
            bind_capture_to_sequence, materialize_with_sequence_guard, persist_if_sequence_holds,
            CapturePrivacyMetadata, SequenceBindError, SequenceBoundCapture,
        };
        use std::sync::atomic::{AtomicU32, Ordering};

        const PASSWORD: &str = "hunter2-from-the-next-copy";

        #[test]
        fn a_sequence_change_after_metadata_does_not_persist_sensitive_content() {
            let sequence = AtomicU32::new(10);
            let persisted = std::cell::RefCell::new(None);
            let bound = bind_capture_to_sequence(
                || sequence.load(Ordering::SeqCst),
                || {
                    let metadata = CapturePrivacyMetadata { sensitive: false };
                    sequence.store(11, Ordering::SeqCst);
                    metadata
                },
                || PASSWORD,
            );
            if let SequenceBoundCapture::Bound { payload, .. } = bound {
                *persisted.borrow_mut() = Some(payload);
            }
            assert!(matches!(
                bound,
                SequenceBoundCapture::Changed { from: 10, to: 11 }
            ));
            assert!(
                persisted.borrow().is_none(),
                "sensitive content from the next sequence must not be persisted"
            );
        }

        #[test]
        fn a_sequence_change_during_delayed_materialize_does_not_persist_sensitive_content() {
            let sequence = AtomicU32::new(20);
            let attempts = AtomicU32::new(0);
            let result = materialize_with_sequence_guard(
                20,
                || sequence.load(Ordering::SeqCst),
                |_| {
                    let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                    if attempt == 0 {
                        sequence.store(21, Ordering::SeqCst);
                        None
                    } else {
                        Some(PASSWORD)
                    }
                },
                4,
            );
            assert!(matches!(
                result,
                Err(SequenceBindError::Changed { from: 20, to: 21 })
            ));
            assert_ne!(result.ok().flatten(), Some(PASSWORD));
        }

        #[test]
        fn persist_is_rejected_when_the_sequence_moves_before_write() {
            assert_eq!(
                persist_if_sequence_holds(3, 4, PASSWORD),
                Err(SequenceBindError::Changed { from: 3, to: 4 })
            );
            assert_eq!(persist_if_sequence_holds(3, 3, "safe"), Ok("safe"));
        }

        #[test]
        fn a_stable_sequence_keeps_metadata_and_payload_together() {
            let bound = bind_capture_to_sequence(
                || 7,
                || CapturePrivacyMetadata { sensitive: true },
                || "tagged-secret",
            );
            match bound {
                SequenceBoundCapture::Bound {
                    sequence,
                    metadata,
                    payload,
                } => {
                    assert_eq!(sequence, 7);
                    assert!(metadata.sensitive);
                    assert_eq!(payload, "tagged-secret");
                }
                SequenceBoundCapture::Changed { from, to } => {
                    panic!("stable sequence changed {from} -> {to}")
                }
            }
        }
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn an_undecodable_image_fails_before_the_clipboard_is_touched() {
        // Preparation has to reject bad bytes on its own, because
        // set_clipboard_image_png empties the clipboard only once this returns
        // Ok. Moving the decode back inside the open handle would leave the
        // user with an empty clipboard and nothing written, and would keep
        // every other test in this module green.
        assert!(super::prepare_image_clipboard_formats(b"\x89PNG\r\n\x1a\nfake").is_err());
    }

    #[test]
    fn the_relay_ignore_hash_describes_the_write_not_the_capture() {
        let fragment_html = b"<b>hi</b>".to_vec();
        let files = b"C:\\secret.txt".to_vec();
        let captured = calculate_hash(&build_clip_hash_material(
            "text",
            b"hi",
            [
                ("html", fragment_html.as_slice()),
                ("files", files.as_slice()),
            ],
        ));
        let relayed = relayed_clip_hash(
            "text",
            b"hi",
            None,
            &[
                CapturedFormat {
                    name: "html",
                    content: fragment_html.clone(),
                },
                CapturedFormat {
                    name: "files",
                    content: files.clone(),
                },
            ],
        );

        // A bare fragment is wrapped by cf_html::document on the way out, and
        // "files" is never written, so a re-capture reads back neither.
        assert_ne!(captured, relayed);

        let normalized =
            crate::cf_html::document(&String::from_utf8_lossy(&fragment_html)).into_bytes();
        assert_eq!(
            relayed,
            calculate_hash(&build_clip_hash_material(
                "text",
                b"hi",
                [("html", normalized.as_slice())]
            ))
        );
    }

    #[test]
    fn an_image_relay_hash_is_unchanged_by_normalization() {
        // Images republish the same PNG bytes and exclude format material, so
        // the predicted hash must still match the captured one.
        let png = b"\x89PNG\r\n\x1a\nfake".to_vec();
        let captured = calculate_hash(&build_clip_hash_material("image", &png, std::iter::empty()));
        assert_eq!(captured, relayed_clip_hash("image", b"", Some(&png), &[]));
    }

    #[test]
    fn an_ignored_viewer_is_not_relayed_either() {
        // Naming an app in ignored_apps keeps Cubby out of it entirely, and is
        // the only way to disable the relay for one viewer but not another.
        assert!(!should_relay_capture(
            true,
            is_remote_client_owner(Some("ncplayer.exe"), true),
            true
        ));
    }

    #[test]
    fn an_unattributed_owner_is_never_treated_as_remote() {
        // Attribution falls back to the foreground window when
        // GetClipboardOwner fails. A password manager writing without claiming
        // ownership while a remote client is focused must not be relayed into
        // remote sessions, nor bypass skip_sensitive on the way into history.
        assert!(!is_remote_client_owner(Some("ncplayer.exe"), false));
        assert!(!is_remote_client_owner(Some("mstsc.exe"), false));
        assert!(!should_relay_capture(
            true,
            is_remote_client_owner(Some("ncplayer.exe"), false),
            false
        ));
        assert!(should_skip_sensitive_capture(
            true,
            true,
            is_remote_client_owner(Some("ncplayer.exe"), false)
        ));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn the_sensitive_marker_payload_is_never_empty() {
        // clipboard_win::raw::set_inner returns Ok without calling
        // SetClipboardData for a zero-length payload, so an empty marker
        // reports success while writing nothing and silently publishes tagged
        // content untagged.
        assert!(!super::SENSITIVE_MARKER_PAYLOAD.is_empty());
    }

    #[test]
    fn a_remote_clients_sensitive_marker_does_not_suppress_storage() {
        // The viewer tags everything it forwards, so honoring it here dropped
        // every remote-session copy and left a hole in history (SBS-781).
        assert!(!should_skip_sensitive_capture(true, true, true));
    }

    #[test]
    fn a_local_apps_sensitive_marker_still_suppresses_storage() {
        // The whole point of scoping the bypass: a password manager sets this
        // deliberately, about content it owns, and must keep being honored.
        assert!(should_skip_sensitive_capture(true, true, false));
    }

    #[test]
    fn untagged_content_and_the_disabled_setting_never_skip() {
        assert!(!should_skip_sensitive_capture(true, false, false));
        assert!(!should_skip_sensitive_capture(false, true, false));
        assert!(!should_skip_sensitive_capture(false, true, true));
    }

    /// SBS-922: Skip likely secrets must refuse a 9 KiB paste that starts
    /// with a known marker, not store it because the whole blob is over 8 KiB.
    #[test]
    fn skip_likely_secrets_refuses_a_9kib_prefix_secret() {
        let padding = "x".repeat(9 * 1024);
        let github = format!("ghp_{} {padding}", "abcdefghijklmnopqrstuvwxyz0123456789");
        let aws = format!("AKIA{}{} {padding}", "IOSFODNN7", "EXAMPLE");
        let pem = format!(
            "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA\n-----END RSA PRIVATE KEY-----\n{padding}"
        );
        assert!(matches!(
            likely_secret_decision(true, "text", github.as_bytes()),
            LikelySecretDecision::Skip(crate::secrets::SecretKind::GitHubToken)
        ));
        assert!(matches!(
            likely_secret_decision(true, "text", aws.as_bytes()),
            LikelySecretDecision::Skip(crate::secrets::SecretKind::AwsAccessKey)
        ));
        assert!(matches!(
            likely_secret_decision(true, "text", pem.as_bytes()),
            LikelySecretDecision::Skip(crate::secrets::SecretKind::PrivateKey)
        ));
        assert_eq!(
            likely_secret_decision(true, "text", padding.as_bytes()),
            LikelySecretDecision::Store
        );
        // Setting off, or a non-text clip, still stores.
        assert_eq!(
            likely_secret_decision(false, "text", github.as_bytes()),
            LikelySecretDecision::Store
        );
        assert_eq!(
            likely_secret_decision(true, "image", github.as_bytes()),
            LikelySecretDecision::Store
        );
    }

    /// A scan error is not "not a secret". Invalid UTF-8 text cannot be
    /// classified, so capture skips storage when the setting is on.
    #[test]
    fn skip_likely_secrets_treats_undecodable_text_as_unscannable() {
        assert_eq!(
            likely_secret_decision(true, "text", &[0xff, 0xfe, 0xfd]),
            LikelySecretDecision::Unscannable
        );
        assert_eq!(
            likely_secret_decision(false, "text", &[0xff, 0xfe, 0xfd]),
            LikelySecretDecision::Store
        );
    }

    #[test]
    fn only_remote_clients_bypass_the_sensitive_marker() {
        // The bypass must never reach a local application: a password manager
        // sets this marker deliberately and about content it actually owns.
        assert!(is_remote_client_process(Some("ncplayer.exe")));
        assert!(is_remote_client_process(Some("mstsc.exe")));
        assert!(!is_remote_client_process(Some("1Password.exe")));
        assert!(!is_remote_client_process(Some("notepad.exe")));
        assert!(!is_remote_client_process(None));
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
        assert_eq!(
            IGNORE_HASH.lock().as_ref().map(|(hash, _)| hash.as_str()),
            Some("expected")
        );

        clear_ignore_hash_if_matches("expected");
        assert!(IGNORE_HASH.lock().is_none());
    }

    #[test]
    fn ignore_marker_applies_to_a_fresh_matching_write() {
        let now = Instant::now();
        let marker = ("hash-a".to_string(), now);
        assert!(ignore_marker_applies(
            Some(&marker),
            "hash-a",
            now + Duration::from_millis(50),
            IGNORE_HASH_TTL
        ));
    }

    #[test]
    fn ignore_marker_never_applies_to_other_content() {
        let now = Instant::now();
        let marker = ("hash-a".to_string(), now);
        assert!(!ignore_marker_applies(
            Some(&marker),
            "hash-b",
            now,
            IGNORE_HASH_TTL
        ));
        assert!(!ignore_marker_applies(None, "hash-a", now, IGNORE_HASH_TTL));
    }

    #[test]
    fn a_stale_marker_stops_swallowing_real_copies() {
        // The self-write was never observed (contended read, or a remote client
        // rewrote the clipboard first). Copying that same content later is a
        // genuine copy and must still be captured.
        let now = Instant::now();
        let marker = ("hash-a".to_string(), now);
        assert!(!ignore_marker_applies(
            Some(&marker),
            "hash-a",
            now + IGNORE_HASH_TTL + Duration::from_millis(1),
            IGNORE_HASH_TTL
        ));
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

    /// SBS-769: consecutive-hash equality used to skip capture unconditionally.
    /// After retention expires an original, that skip is the revival miss.
    #[test]
    fn consecutive_dedup_does_not_skip_an_expired_or_unknown_original() {
        use super::{consecutive_dedup_decision, ConsecutiveDedupDecision, StoredOriginalLookup};

        // Today's helper: hash match => skip. Expired must not take that path.
        assert_eq!(
            consecutive_dedup_decision(StoredOriginalLookup::Found {
                full_image_expired: true,
            }),
            ConsecutiveDedupDecision::Capture
        );
        assert_eq!(
            consecutive_dedup_decision(StoredOriginalLookup::Unknown),
            ConsecutiveDedupDecision::Capture
        );
        assert_eq!(
            consecutive_dedup_decision(StoredOriginalLookup::Found {
                full_image_expired: false,
            }),
            ConsecutiveDedupDecision::Skip
        );
        assert_eq!(
            consecutive_dedup_decision(StoredOriginalLookup::Missing),
            ConsecutiveDedupDecision::Skip
        );
    }

    mod revive_expired_original {
        use super::super::{
            consecutive_dedup_decision, lookup_stored_original, persist_full_image_file,
            recapture_existing_image, ConsecutiveDedupDecision, StoredOriginalLookup,
        };
        use crate::commands::{load_full_image_content, IMAGE_EXPIRED_ERROR};
        use crate::database::Database;
        use crate::image_persist::RecaptureFields;
        use crate::models::Clip;
        use sqlx::sqlite::SqlitePoolOptions;
        use std::sync::Arc;

        const OCR_TEXT: &str = "invoice AK1A9";
        const OCR_WORDS: &str = r#"{"image_width":10,"image_height":10,"words":[]}"#;
        const THUMBNAIL: &[u8] = b"thumbnail-bytes";
        const ORIGINAL: &[u8] = b"full-resolution-png-bytes";

        async fn test_database() -> Database {
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect("sqlite::memory:")
                .await
                .expect("in-memory database should open");
            sqlx::query("PRAGMA foreign_keys = ON")
                .execute(&pool)
                .await
                .expect("foreign keys should be enabled in tests");
            let database = Database {
                pool,
                crypto: Arc::new(crate::crypto::CryptoManager::ephemeral()),
                image_dir: std::env::temp_dir()
                    .join(format!("cubby-sbs-769-{}", uuid::Uuid::new_v4())),
                search_index: Arc::new(crate::search_index::SearchIndex::default()),
            };
            database.migrate().await.expect("migration should succeed");
            database
        }

        async fn seed_expired_image(database: &Database, uuid: &str, content_hash: &str) {
            sqlx::query(
                r#"
                INSERT INTO clips (
                    uuid, clip_type, content, text_preview, content_hash,
                    ocr_text, ocr_words, ocr_status, full_image_expired, is_thumbnail
                )
                VALUES (?, 'image', ?, '[Image]', ?, ?, ?, 'completed', 1, 1)
                "#,
            )
            .bind(uuid)
            .bind(THUMBNAIL)
            .bind(content_hash)
            .bind(OCR_TEXT)
            .bind(OCR_WORDS)
            .execute(&database.pool)
            .await
            .expect("expired image fixture should insert");
        }

        async fn expired_flag(database: &Database, uuid: &str) -> i64 {
            sqlx::query_scalar("SELECT full_image_expired FROM clips WHERE uuid = ?")
                .bind(uuid)
                .fetch_one(&database.pool)
                .await
                .expect("expired flag should load")
        }

        async fn ocr_row(database: &Database, uuid: &str) -> (String, String, String) {
            sqlx::query_as("SELECT ocr_text, ocr_words, ocr_status FROM clips WHERE uuid = ?")
                .bind(uuid)
                .fetch_one(&database.pool)
                .await
                .expect("ocr columns should load")
        }

        async fn load_clip(database: &Database, uuid: &str) -> Clip {
            sqlx::query_as("SELECT * FROM clips WHERE uuid = ?")
                .bind(uuid)
                .fetch_one(&database.pool)
                .await
                .expect("clip should load")
        }

        fn cleanup(database: &Database) {
            let _ = std::fs::remove_dir_all(&database.image_dir);
        }

        /// The recapture the capture loop performs for an existing image clip:
        /// stage the original, rewrite the row, index it, then swap the file in.
        async fn recapture(database: &Database, uuid: &str, bytes: &[u8]) -> Result<(), String> {
            recapture_existing_image(
                database,
                uuid,
                bytes,
                RecaptureFields {
                    source_app: &None,
                    source_icon: &None,
                    content: THUMBNAIL,
                    preview: "[Image]",
                    metadata: None,
                },
            )
            .await
        }

        /// SBS-769: re-copying an expired image wrote fresh bytes but left
        /// `full_image_expired` set, so paste/copy still rejected the original.
        #[tokio::test]
        async fn direct_recopy_after_expiry_restores_original_and_keeps_ocr() {
            let database = test_database().await;
            seed_expired_image(&database, "shot", "hash-shot").await;

            let mut expired = load_clip(&database, "shot").await;
            assert_eq!(
                load_full_image_content(&database, &mut expired)
                    .await
                    .unwrap_err(),
                IMAGE_EXPIRED_ERROR
            );

            recapture(&database, "shot", ORIGINAL)
                .await
                .expect("revival should persist the original");

            assert_eq!(expired_flag(&database, "shot").await, 0);
            let mut revived = load_clip(&database, "shot").await;
            let bytes = load_full_image_content(&database, &mut revived)
                .await
                .expect("revived original should be loadable");
            assert_eq!(bytes, ORIGINAL);

            let (ocr_text, ocr_words, ocr_status) = ocr_row(&database, "shot").await;
            assert_eq!(ocr_text, OCR_TEXT);
            assert_eq!(ocr_words, OCR_WORDS);
            assert_eq!(ocr_status, "completed");
            let thumbnail: Vec<u8> =
                sqlx::query_scalar("SELECT content FROM clips WHERE uuid = 'shot'")
                    .fetch_one(&database.pool)
                    .await
                    .expect("thumbnail should remain");
            assert_eq!(thumbnail, THUMBNAIL);

            cleanup(&database);
        }

        /// SBS-769: LAST_STABLE_HASH matching used to drop the recapture
        /// entirely after expiry, so a second consecutive copy never revived.
        #[tokio::test]
        async fn consecutive_duplicate_after_expiry_does_not_skip_revival() {
            let database = test_database().await;
            seed_expired_image(&database, "shot", "hash-shot").await;

            assert_eq!(
                lookup_stored_original(&database.pool, "hash-shot").await,
                StoredOriginalLookup::Found {
                    full_image_expired: true,
                }
            );
            assert_eq!(
                lookup_stored_original(&database.pool, "no-such-hash").await,
                StoredOriginalLookup::Missing
            );
            // Today's hash-only helper would Skip here and never restore.
            assert_eq!(
                consecutive_dedup_decision(StoredOriginalLookup::Found {
                    full_image_expired: true,
                }),
                ConsecutiveDedupDecision::Capture
            );

            recapture(&database, "shot", ORIGINAL)
                .await
                .expect("revival after a consecutive match should persist");

            assert_eq!(expired_flag(&database, "shot").await, 0);
            assert_eq!(
                lookup_stored_original(&database.pool, "hash-shot").await,
                StoredOriginalLookup::Found {
                    full_image_expired: false,
                }
            );
            let mut revived = load_clip(&database, "shot").await;
            let bytes = load_full_image_content(&database, &mut revived)
                .await
                .expect("consecutive revival should make the original usable");
            assert_eq!(bytes, ORIGINAL);
            assert_eq!(
                consecutive_dedup_decision(StoredOriginalLookup::Found {
                    full_image_expired: false,
                }),
                ConsecutiveDedupDecision::Skip
            );

            cleanup(&database);
        }

        /// SBS-769: a failed persist or a failed clip_images index must leave
        /// `full_image_expired` set so copy/paste do not claim a missing original.
        #[tokio::test]
        async fn persist_or_index_failure_does_not_clear_expired_flag() {
            let persist_fail_db = test_database().await;
            seed_expired_image(&persist_fail_db, "shot", "hash-shot").await;
            std::fs::write(&persist_fail_db.image_dir, b"not-a-directory")
                .expect("image_dir should be a file so persist cannot create it");

            let persist_err = recapture(&persist_fail_db, "shot", ORIGINAL)
                .await
                .expect_err("staging must fail when image_dir is a file");
            assert!(
                persist_err.contains("persist"),
                "the error should describe the failed write: {persist_err}"
            );
            assert_eq!(expired_flag(&persist_fail_db, "shot").await, 1);
            let persist_index_rows: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM clip_images WHERE clip_uuid = 'shot'")
                    .fetch_one(&persist_fail_db.pool)
                    .await
                    .expect("clip_images count should load");
            assert_eq!(persist_index_rows, 0);
            let _ = std::fs::remove_file(&persist_fail_db.image_dir);

            let index_fail_db = test_database().await;
            seed_expired_image(&index_fail_db, "shot", "hash-shot").await;
            sqlx::query("DROP TABLE clip_images")
                .execute(&index_fail_db.pool)
                .await
                .expect("dropping clip_images should force an index failure");

            let index_err = recapture(&index_fail_db, "shot", ORIGINAL)
                .await
                .expect_err("index must fail when clip_images is missing");
            assert!(
                index_err.contains("index"),
                "the error should describe the failed index: {index_err}"
            );
            assert_eq!(expired_flag(&index_fail_db, "shot").await, 1);
            // Review finding on PR #214: the write must not leave an orphan
            // behind. Staging handles that on its own -- the temp file is only
            // renamed onto `{uuid}.cubby` after the transaction commits, so a
            // failed index leaves neither an original nor a stray temp.
            assert!(
                !index_fail_db.image_dir.join("shot.cubby").exists(),
                "a failed index must not leave an unindexed original behind"
            );
            assert!(
                !index_fail_db.image_dir.join("shot.cubby.tmp").exists(),
                "a failed index must not leave the staged temp file behind"
            );

            cleanup(&index_fail_db);
        }

        /// Review finding on PR #214: a non-consecutive duplicate re-copy
        /// reaches the recapture with `full_image_expired` already 0, so the
        /// path can hold a valid original that a live `clip_images` row points
        /// at. A failed recapture must leave that file exactly as it was
        /// rather than stranding the row that describes it.
        #[tokio::test]
        async fn a_failed_recapture_does_not_disturb_a_preexisting_original() {
            let database = test_database().await;
            seed_expired_image(&database, "shot", "hash-shot").await;
            // Clear the expired flag to model an original that is already
            // valid, then write the file this recapture would replace.
            sqlx::query("UPDATE clips SET full_image_expired = 0 WHERE uuid = 'shot'")
                .execute(&database.pool)
                .await
                .expect("clearing the expired flag should succeed");
            let existing_path =
                persist_full_image_file(&database.crypto, &database.image_dir, "shot", ORIGINAL)
                    .expect("seeding the pre-existing original should succeed");
            let existing_bytes =
                std::fs::read(&existing_path).expect("the seeded original should be readable");

            sqlx::query("DROP TABLE clip_images")
                .execute(&database.pool)
                .await
                .expect("dropping clip_images should force an index failure");

            let index_err = recapture(
                &database,
                "shot",
                b"different-bytes-from-a-racing-recapture",
            )
            .await
            .expect_err("index must fail when clip_images is missing");
            assert!(
                index_err.contains("index"),
                "the error should describe the failed index: {index_err}"
            );
            assert_eq!(
                std::fs::read(&existing_path).expect("the original must still be there"),
                existing_bytes,
                "a file that already existed must survive a failed recapture untouched"
            );

            cleanup(&database);
        }

        /// The whole point of SBS-769, end to end: one recapture is what makes
        /// an expired clip usable again. Nothing runs after it to clear the
        /// flag, so the row, the index, and the file must all be correct when
        /// that single call returns.
        #[tokio::test]
        async fn one_recapture_leaves_the_clip_fully_usable() {
            let database = test_database().await;
            seed_expired_image(&database, "shot", "hash-shot").await;

            recapture(&database, "shot", ORIGINAL)
                .await
                .expect("the recapture should succeed");

            assert_eq!(
                expired_flag(&database, "shot").await,
                0,
                "the recapture transaction must clear the expired flag itself"
            );
            let indexed: (String, i64) = sqlx::query_as(
                "SELECT file_path, file_size FROM clip_images WHERE clip_uuid = 'shot'",
            )
            .fetch_one(&database.pool)
            .await
            .expect("the recapture should have indexed the original");
            assert_eq!(indexed.1, ORIGINAL.len() as i64);
            assert!(
                std::path::Path::new(&indexed.0).exists(),
                "clip_images must point at a file that is actually on disk"
            );
            let mut revived = load_clip(&database, "shot").await;
            assert_eq!(
                load_full_image_content(&database, &mut revived)
                    .await
                    .expect("Paste and Copy must work after one recapture"),
                ORIGINAL
            );

            cleanup(&database);
        }
    }
}
