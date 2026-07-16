#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("clipboard_probe only runs on Windows");
    std::process::exit(1);
}

#[cfg(target_os = "windows")]
mod windows_probe {
    use clipboard_rs::common::RustImage;
    use clipboard_rs::{Clipboard, ClipboardContext};
    use serde::Serialize;
    use sha2::{Digest, Sha256};
    use std::collections::HashSet;
    use std::env;
    use std::process::{Child, Command};
    use std::sync::{Mutex, OnceLock};
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
    use windows::core::w;
    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::DataExchange::{
        AddClipboardFormatListener, CloseClipboard, EnumClipboardFormats, GetClipboardFormatNameW,
        GetClipboardSequenceNumber, OpenClipboard, RemoveClipboardFormatListener,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
        PostQuitMessage, RegisterClassW, SetTimer, TranslateMessage, CS_HREDRAW, CS_VREDRAW,
        CW_USEDEFAULT, MSG, WINDOW_EX_STYLE, WINDOW_STYLE, WM_CLIPBOARDUPDATE, WM_DESTROY,
        WM_TIMER, WNDCLASSW,
    };

    const TIMER_ID: usize = 1;
    const DEFAULT_TIMEOUT_SECONDS: u64 = 30;

    static STATE: OnceLock<Mutex<ProbeState>> = OnceLock::new();

    #[derive(Debug)]
    struct Config {
        burst_count: Option<usize>,
        interval_ms: u64,
        contention_ms: u64,
        timeout_seconds: u64,
        expect_text: Option<usize>,
        expect_items: Option<usize>,
        writer: bool,
    }

    #[derive(Debug)]
    struct ProbeState {
        started: Instant,
        expected: Option<HashSet<String>>,
        observed: HashSet<String>,
        expected_text: Option<usize>,
        observed_text: HashSet<String>,
        expected_items: Option<usize>,
        observed_items: HashSet<String>,
        events: usize,
        read_failures: usize,
        timed_out: bool,
    }

    #[derive(Serialize)]
    struct ClipboardEvent {
        event: &'static str,
        timestamp_ms: u128,
        elapsed_ms: u128,
        sequence: u32,
        formats: Vec<String>,
        text_length: Option<usize>,
        text_sha256: Option<String>,
        text_status: &'static str,
        text_read_error: Option<String>,
        image_width: Option<u32>,
        image_height: Option<u32>,
        image_sha256: Option<String>,
        image_status: &'static str,
        image_read_error: Option<String>,
        marker: Option<String>,
        read_error: Option<String>,
    }

    struct ImageSnapshot {
        width: u32,
        height: u32,
        sha256: String,
    }

    pub fn run() {
        let config = parse_args();
        if config.writer {
            run_burst_writer(
                config
                    .burst_count
                    .expect("internal writer requires --burst"),
                config.interval_ms,
                config.contention_ms,
            );
            return;
        }

        let expected = config.burst_count.map(|count| {
            (0..count)
                .map(|index| burst_marker(index, count))
                .collect::<HashSet<_>>()
        });

        STATE
            .set(Mutex::new(ProbeState {
                started: Instant::now(),
                expected,
                observed: HashSet::new(),
                expected_text: config.expect_text,
                observed_text: HashSet::new(),
                expected_items: config.expect_items,
                observed_items: HashSet::new(),
                events: 0,
                read_failures: 0,
                timed_out: false,
            }))
            .expect("clipboard probe state should initialize once");

        let hwnd = create_listener_window().unwrap_or_else(|error| {
            eprintln!("failed to create clipboard listener window: {error}");
            std::process::exit(1);
        });

        unsafe {
            AddClipboardFormatListener(hwnd).unwrap_or_else(|error| {
                eprintln!("failed to register clipboard listener: {error}");
                std::process::exit(1);
            });
            SetTimer(
                Some(hwnd),
                TIMER_ID,
                (config.timeout_seconds * 1000) as u32,
                None,
            );
        }

        println!(
            "{}",
            serde_json::json!({
                "event": "ready",
                "mode": if config.burst_count.is_some() {
                    "burst"
                } else if config.expect_text.is_some() {
                    "remote_text"
                } else if config.expect_items.is_some() {
                    "remote_items"
                } else {
                    "interactive"
                },
                "expected_distinct_text": config.expect_text,
                "expected_distinct_items": config.expect_items,
                "contention_ms": config.contention_ms,
                "timeout_seconds": config.timeout_seconds
            })
        );

        let mut writer = config
            .burst_count
            .map(|count| spawn_burst_writer(count, config.interval_ms, config.contention_ms));

        unsafe {
            let mut message = MSG::default();
            while GetMessageW(&mut message, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            let _ = RemoveClipboardFormatListener(hwnd);
            let _ = DestroyWindow(hwnd);
        }
        wait_for_writer(&mut writer);

        let state = STATE
            .get()
            .expect("state exists")
            .lock()
            .expect("state lock");
        let expected_count = state.expected.as_ref().map_or(0, HashSet::len);
        let observed_count = state.observed.len();
        let expected_text_count = state.expected_text.unwrap_or(0);
        let observed_text_count = state.observed_text.len();
        let expected_item_count = state.expected_items.unwrap_or(0);
        let observed_item_count = state.observed_items.len();
        let burst_passed = state.expected.is_none() || observed_count == expected_count;
        let text_passed =
            state.expected_text.is_none() || observed_text_count >= expected_text_count;
        let items_passed =
            state.expected_items.is_none() || observed_item_count >= expected_item_count;
        let has_expectations = state.expected.is_some()
            || state.expected_text.is_some()
            || state.expected_items.is_some();
        let passed = (!has_expectations || !state.timed_out)
            && burst_passed
            && text_passed
            && items_passed
            && state.read_failures == 0;

        println!(
            "{}",
            serde_json::json!({
                "event": "summary",
                "passed": passed,
                "events": state.events,
                "read_failures": state.read_failures,
                "expected_markers": expected_count,
                "observed_markers": observed_count,
                "expected_distinct_text": expected_text_count,
                "observed_distinct_text": observed_text_count,
                "expected_distinct_items": expected_item_count,
                "observed_distinct_items": observed_item_count,
                "timed_out": state.timed_out
            })
        );

        if !passed {
            std::process::exit(2);
        }
    }

    fn parse_args() -> Config {
        let mut burst_count = None;
        let mut interval_ms = 25;
        let mut contention_ms = 0;
        let mut timeout_seconds = DEFAULT_TIMEOUT_SECONDS;
        let mut expect_text = None;
        let mut expect_items = None;
        let mut writer = false;
        let mut args = env::args().skip(1);

        while let Some(argument) = args.next() {
            match argument.as_str() {
                "--burst" => burst_count = Some(parse_value(&mut args, "--burst")),
                "--interval-ms" => {
                    interval_ms = parse_value(&mut args, "--interval-ms");
                }
                "--contention-ms" => {
                    contention_ms = parse_value(&mut args, "--contention-ms");
                }
                "--timeout-seconds" => {
                    timeout_seconds = parse_value(&mut args, "--timeout-seconds");
                }
                "--expect-text" => {
                    expect_text = Some(parse_value(&mut args, "--expect-text"));
                }
                "--expect-items" => {
                    expect_items = Some(parse_value(&mut args, "--expect-items"));
                }
                "--writer" => writer = true,
                "--help" | "-h" => {
                    println!(
                        "Usage: clipboard_probe [--burst COUNT] [--interval-ms MS] [--contention-ms MS] [--expect-text COUNT] [--expect-items COUNT] [--timeout-seconds SECONDS]"
                    );
                    std::process::exit(0);
                }
                _ => {
                    eprintln!("unknown argument: {argument}");
                    std::process::exit(1);
                }
            }
        }

        Config {
            burst_count,
            interval_ms,
            contention_ms,
            timeout_seconds,
            expect_text,
            expect_items,
            writer,
        }
    }

    fn parse_value<T: std::str::FromStr>(
        args: &mut impl Iterator<Item = String>,
        argument: &str,
    ) -> T {
        args.next()
            .unwrap_or_else(|| {
                eprintln!("{argument} requires a value");
                std::process::exit(1);
            })
            .parse()
            .unwrap_or_else(|_| {
                eprintln!("invalid value for {argument}");
                std::process::exit(1);
            })
    }

    fn run_burst_writer(count: usize, interval_ms: u64, contention_ms: u64) {
        thread::sleep(Duration::from_millis(250));
        let clipboard = ClipboardContext::new().unwrap_or_else(|error| {
            eprintln!("failed to create burst clipboard context: {error}");
            std::process::exit(1);
        });

        for index in 0..count {
            let marker = burst_marker(index, count);
            let mut last_error = None;

            for attempt in 0..10_u32 {
                match clipboard.set_text(marker.clone()) {
                    Ok(()) => {
                        last_error = None;
                        break;
                    }
                    Err(error) => {
                        last_error = Some(error.to_string());
                        thread::sleep(Duration::from_millis(2_u64.pow(attempt.min(6))));
                    }
                }
            }

            if let Some(error) = last_error {
                eprintln!("failed to write burst marker {index}: {error}");
                break;
            }

            if contention_ms > 0 {
                hold_clipboard_open(contention_ms).unwrap_or_else(|error| {
                    eprintln!(
                        "failed to create clipboard contention after marker {index}: {error}"
                    );
                    std::process::exit(1);
                });
            }

            thread::sleep(Duration::from_millis(interval_ms));
        }
    }

    fn hold_clipboard_open(contention_ms: u64) -> Result<(), String> {
        let mut last_error = None;
        for attempt in 0..10_u32 {
            match unsafe { OpenClipboard(None) } {
                Ok(()) => {
                    thread::sleep(Duration::from_millis(contention_ms));
                    unsafe {
                        CloseClipboard().map_err(|error| error.to_string())?;
                    }
                    return Ok(());
                }
                Err(error) => {
                    last_error = Some(error.to_string());
                    thread::sleep(Duration::from_millis(1_u64 << attempt.min(6)));
                }
            }
        }

        Err(last_error.unwrap_or_else(|| "unknown clipboard lock error".to_string()))
    }

    fn spawn_burst_writer(count: usize, interval_ms: u64, contention_ms: u64) -> Child {
        Command::new(env::current_exe().expect("resolve clipboard probe executable"))
            .args([
                "--writer",
                "--burst",
                &count.to_string(),
                "--interval-ms",
                &interval_ms.to_string(),
                "--contention-ms",
                &contention_ms.to_string(),
            ])
            .spawn()
            .unwrap_or_else(|error| {
                eprintln!("failed to start clipboard writer process: {error}");
                std::process::exit(1);
            })
    }

    fn wait_for_writer(writer: &mut Option<Child>) {
        if let Some(writer) = writer {
            if let Err(error) = writer.wait() {
                eprintln!("failed to wait for clipboard writer process: {error}");
            }
        }
    }

    fn burst_marker(index: usize, count: usize) -> String {
        format!("CUBBY-PROBE-{index:04}-OF-{count:04}")
    }

    fn create_listener_window() -> windows::core::Result<HWND> {
        unsafe {
            let module = GetModuleHandleW(None)?;
            let instance = HINSTANCE(module.0);
            let class_name = w!("CubbyClipboardProbeListener");
            let window_class = WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                lpszClassName: class_name,
                ..Default::default()
            };

            if RegisterClassW(&window_class) == 0 {
                return Err(windows::core::Error::from_thread());
            }

            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                w!("Cubby Clipboard Probe"),
                WINDOW_STYLE::default(),
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                0,
                0,
                None,
                None,
                Some(instance),
                None,
            )
        }
    }

    unsafe extern "system" fn window_proc(
        hwnd: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            WM_CLIPBOARDUPDATE => {
                capture_event();
                LRESULT(0)
            }
            WM_TIMER if wparam.0 == TIMER_ID => {
                if let Some(state) = STATE.get() {
                    state.lock().expect("state lock").timed_out = true;
                }
                PostQuitMessage(0);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }

    unsafe fn capture_event() {
        let sequence = GetClipboardSequenceNumber();
        let (formats, format_error) = read_formats_with_retry();
        let has_unicode_text = formats.iter().any(|format| format == "CF_UNICODETEXT");
        let (text, attempted_text_error) = read_text_with_retry();
        let text_error = has_unicode_text
            .then_some(attempted_text_error.clone())
            .flatten();
        let (image, attempted_image_error) = if text.is_none() {
            read_image_with_retry()
        } else {
            (None, None)
        };
        let read_error = match (format_error, text_error) {
            (None, None) => None,
            (Some(error), None) | (None, Some(error)) => Some(error),
            (Some(first), Some(second)) => Some(format!("{first}; {second}")),
        };
        let marker = text
            .as_deref()
            .filter(|value| value.starts_with("CUBBY-PROBE-"))
            .map(str::to_owned);
        let text_sha256 = text.as_ref().map(|value| {
            let mut hasher = Sha256::new();
            hasher.update(value.as_bytes());
            format!("{:x}", hasher.finalize())
        });
        let text_status = match (&text, has_unicode_text, &attempted_text_error) {
            (Some(_), _, _) => "readable",
            (None, true, Some(_)) => "advertised_but_unreadable",
            (None, false, Some(_)) => "not_available",
            (None, _, None) => "not_text",
        };
        let image_status = match (&image, &attempted_image_error) {
            (Some(_), _) => "readable",
            (None, Some(_)) => "not_available",
            (None, None) => "not_checked",
        };

        let mut should_quit = false;
        let elapsed_ms = if let Some(state_cell) = STATE.get() {
            let mut state = state_cell.lock().expect("state lock");
            state.events += 1;
            if read_error.is_some() {
                state.read_failures += 1;
            }
            if let Some(marker) = marker.as_ref() {
                if state
                    .expected
                    .as_ref()
                    .is_some_and(|expected| expected.contains(marker))
                {
                    state.observed.insert(marker.clone());
                }
            }
            if let Some(text_sha256) = text_sha256.as_ref() {
                state.observed_text.insert(text_sha256.clone());
                state.observed_items.insert(format!("text:{text_sha256}"));
            } else if let Some(image) = image.as_ref() {
                state
                    .observed_items
                    .insert(format!("image:{}", image.sha256));
            }
            let burst_complete = state
                .expected
                .as_ref()
                .is_some_and(|expected| state.observed.len() == expected.len());
            let text_complete = state
                .expected_text
                .is_some_and(|expected| state.observed_text.len() >= expected);
            let items_complete = state
                .expected_items
                .is_some_and(|expected| state.observed_items.len() >= expected);
            should_quit = burst_complete || text_complete || items_complete;
            state.started.elapsed().as_millis()
        } else {
            0
        };

        let event = ClipboardEvent {
            event: "clipboard_update",
            timestamp_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            elapsed_ms,
            sequence,
            formats,
            text_length: text.as_ref().map(String::len),
            text_sha256,
            text_status,
            text_read_error: attempted_text_error,
            image_width: image.as_ref().map(|snapshot| snapshot.width),
            image_height: image.as_ref().map(|snapshot| snapshot.height),
            image_sha256: image.as_ref().map(|snapshot| snapshot.sha256.clone()),
            image_status,
            image_read_error: attempted_image_error,
            marker,
            read_error,
        };
        println!(
            "{}",
            serde_json::to_string(&event).expect("serialize event")
        );

        if should_quit {
            PostQuitMessage(0);
        }
    }

    unsafe fn read_formats_with_retry() -> (Vec<String>, Option<String>) {
        let mut last_error = None;
        for attempt in 0..10_u32 {
            match OpenClipboard(None) {
                Ok(()) => {
                    let mut formats = Vec::new();
                    let mut format = 0;
                    loop {
                        format = EnumClipboardFormats(format);
                        if format == 0 {
                            break;
                        }
                        formats.push(format_name(format));
                    }
                    let _ = CloseClipboard();
                    return (formats, None);
                }
                Err(error) => {
                    last_error = Some(error.to_string());
                    thread::sleep(Duration::from_millis(2_u64.pow(attempt.min(6))));
                }
            }
        }
        (
            Vec::new(),
            Some(format!(
                "format enumeration failed: {}",
                last_error.unwrap_or_else(|| "unknown error".to_string())
            )),
        )
    }

    fn read_text_with_retry() -> (Option<String>, Option<String>) {
        let clipboard = match ClipboardContext::new() {
            Ok(clipboard) => clipboard,
            Err(error) => return (None, Some(format!("clipboard init failed: {error}"))),
        };
        let mut last_error = None;

        for attempt in 0..10_u32 {
            match clipboard.get_text() {
                Ok(text) => return (Some(text), None),
                Err(error) => {
                    last_error = Some(error.to_string());
                    thread::sleep(Duration::from_millis(2_u64.pow(attempt.min(6))));
                }
            }
        }

        (
            None,
            last_error.map(|error| format!("text materialization failed: {error}")),
        )
    }

    fn read_image_with_retry() -> (Option<ImageSnapshot>, Option<String>) {
        let clipboard = match ClipboardContext::new() {
            Ok(clipboard) => clipboard,
            Err(error) => return (None, Some(format!("clipboard init failed: {error}"))),
        };
        let mut last_error = None;

        for attempt in 0..10_u32 {
            match clipboard.get_image() {
                Ok(image) => {
                    let (width, height) = image.get_size();
                    match image.to_png() {
                        Ok(png) => {
                            let mut hasher = Sha256::new();
                            hasher.update(png.get_bytes());
                            return (
                                Some(ImageSnapshot {
                                    width,
                                    height,
                                    sha256: format!("{:x}", hasher.finalize()),
                                }),
                                None,
                            );
                        }
                        Err(error) => {
                            return (None, Some(format!("image encoding failed: {error}")));
                        }
                    }
                }
                Err(error) => {
                    last_error = Some(error.to_string());
                    thread::sleep(Duration::from_millis(2_u64.pow(attempt.min(6))));
                }
            }
        }

        (
            None,
            last_error.map(|error| format!("image materialization failed: {error}")),
        )
    }

    unsafe fn format_name(format: u32) -> String {
        match format {
            1 => "CF_TEXT".to_string(),
            2 => "CF_BITMAP".to_string(),
            8 => "CF_DIB".to_string(),
            13 => "CF_UNICODETEXT".to_string(),
            15 => "CF_HDROP".to_string(),
            17 => "CF_DIBV5".to_string(),
            _ => {
                let mut buffer = [0_u16; 256];
                let length = GetClipboardFormatNameW(format, &mut buffer);
                if length > 0 {
                    String::from_utf16_lossy(&buffer[..length as usize])
                } else {
                    format!("FORMAT_{format}")
                }
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn main() {
    windows_probe::run();
}
