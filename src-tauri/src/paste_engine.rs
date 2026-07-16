#[cfg(target_os = "windows")]
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU8, Ordering};
use std::time::Duration;

#[cfg(target_os = "windows")]
static PREVIOUS_FOREGROUND_WINDOW: AtomicIsize = AtomicIsize::new(0);
#[cfg(target_os = "windows")]
static PREVIOUS_INPUT_WINDOW: AtomicIsize = AtomicIsize::new(0);
#[cfg(target_os = "windows")]
static PREVIOUS_PASTE_STRATEGY: AtomicU8 = AtomicU8::new(PasteStrategy::Standard as u8);
#[cfg(target_os = "windows")]
static REMOTE_COPY_HINT_SHOWN: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PasteStrategy {
    Standard = 0,
    RemoteSession = 1,
    NinjaRemote = 2,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PasteContext {
    pub target_kind: &'static str,
    pub remote_paste_mode: String,
}

pub fn paste_context(remote_paste_mode: String) -> PasteContext {
    let target_kind = match previous_paste_strategy() {
        PasteStrategy::Standard => "standard",
        PasteStrategy::RemoteSession => "remote",
        PasteStrategy::NinjaRemote => "ninja",
    };

    PasteContext {
        target_kind,
        remote_paste_mode,
    }
}

#[cfg(target_os = "windows")]
pub fn remember_foreground_window(excluded_hwnd: Option<isize>) -> Option<isize> {
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

    let foreground = unsafe { GetForegroundWindow() };
    let value = foreground.0 as isize;
    if value == 0 || Some(value) == excluded_hwnd {
        return None;
    }

    PREVIOUS_FOREGROUND_WINDOW.store(value, Ordering::SeqCst);
    let input_window = focused_input_window(value).unwrap_or(value);
    PREVIOUS_INPUT_WINDOW.store(input_window, Ordering::SeqCst);
    let process_name = process_name_for_window(value);
    let strategy = process_name
        .as_deref()
        .map(paste_strategy_for_process)
        .unwrap_or(PasteStrategy::Standard);
    PREVIOUS_PASTE_STRATEGY.store(strategy as u8, Ordering::SeqCst);
    log::info!(
        "PASTE: remembered target hwnd={value:#x}, input_hwnd={input_window:#x}, process={:?}, strategy={:?}",
        process_name,
        strategy
    );
    Some(value)
}

#[cfg(target_os = "windows")]
pub fn set_previous_foreground_window(hwnd: isize) {
    set_previous_target(hwnd, PasteStrategy::Standard);
}

#[cfg(target_os = "windows")]
pub fn set_previous_target(hwnd: isize, strategy: PasteStrategy) {
    PREVIOUS_FOREGROUND_WINDOW.store(hwnd, Ordering::SeqCst);
    PREVIOUS_INPUT_WINDOW.store(focused_input_window(hwnd).unwrap_or(hwnd), Ordering::SeqCst);
    PREVIOUS_PASTE_STRATEGY.store(strategy as u8, Ordering::SeqCst);
}

#[cfg(target_os = "windows")]
fn focused_input_window(hwnd: isize) -> Option<isize> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetGUIThreadInfo, GetWindowThreadProcessId, GUITHREADINFO,
    };

    unsafe {
        let thread_id = GetWindowThreadProcessId(HWND(hwnd as _), None);
        if thread_id == 0 {
            return None;
        }

        let mut info = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        GetGUIThreadInfo(thread_id, &mut info).ok()?;
        (info.hwndFocus.0 as isize != 0).then_some(info.hwndFocus.0 as isize)
    }
}

#[cfg(target_os = "windows")]
fn process_name_for_window(hwnd: isize) -> Option<String> {
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, HWND};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId;

    unsafe {
        let mut process_id = 0;
        GetWindowThreadProcessId(HWND(hwnd as _), Some(&mut process_id));
        if process_id == 0 {
            return None;
        }

        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id).ok()?;
        let mut buffer = vec![0_u16; 1024];
        let mut length = buffer.len() as u32;
        let result = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        );
        let _ = CloseHandle(process);
        result.ok()?;

        let path = String::from_utf16_lossy(&buffer[..length as usize]);
        std::path::Path::new(&path)
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.to_ascii_lowercase())
    }
}

pub fn paste_strategy_for_process(process_name: &str) -> PasteStrategy {
    if process_name.eq_ignore_ascii_case("ncplayer.exe") {
        return PasteStrategy::NinjaRemote;
    }

    if matches!(
        process_name.to_ascii_lowercase().as_str(),
        "mstsc.exe"
            | "msrdc.exe"
            | "anydesk.exe"
            | "teamviewer.exe"
            | "teamviewer_desktop.exe"
            | "screenconnect.clientservice.exe"
            | "screenconnect.windowsclient.exe"
            | "splashtop.exe"
            | "strwinclt.exe"
            | "rustdesk.exe"
    ) {
        PasteStrategy::RemoteSession
    } else {
        PasteStrategy::Standard
    }
}

#[cfg(target_os = "windows")]
pub fn previous_paste_strategy() -> PasteStrategy {
    match PREVIOUS_PASTE_STRATEGY.load(Ordering::SeqCst) {
        value if value == PasteStrategy::NinjaRemote as u8 => PasteStrategy::NinjaRemote,
        value if value == PasteStrategy::RemoteSession as u8 => PasteStrategy::RemoteSession,
        _ => PasteStrategy::Standard,
    }
}

pub fn paste_settle_delay(strategy: PasteStrategy) -> Duration {
    match strategy {
        PasteStrategy::Standard => Duration::from_millis(100),
        PasteStrategy::RemoteSession => Duration::from_millis(600),
        PasteStrategy::NinjaRemote => Duration::from_millis(75),
    }
}

pub fn should_auto_paste(strategy: PasteStrategy) -> bool {
    strategy != PasteStrategy::NinjaRemote
}

pub fn should_auto_paste_with_mode(strategy: PasteStrategy, remote_paste_mode: &str) -> bool {
    strategy != PasteStrategy::NinjaRemote || remote_paste_mode == "paste_as_keystrokes"
}

#[cfg(target_os = "windows")]
pub fn take_remote_copy_hint() -> bool {
    REMOTE_COPY_HINT_SHOWN
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_ok()
}

#[cfg(target_os = "windows")]
pub fn restore_previous_foreground_window() -> bool {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, IsWindow,
        SetForegroundWindow,
    };

    let value = PREVIOUS_FOREGROUND_WINDOW.load(Ordering::SeqCst);
    if value == 0 {
        log::warn!("FOCUS: no previous foreground window is available");
        return false;
    }

    let target = HWND(value as _);
    if !unsafe { IsWindow(Some(target)).as_bool() } {
        log::warn!("FOCUS: remembered foreground window is no longer valid");
        PREVIOUS_FOREGROUND_WINDOW.store(0, Ordering::SeqCst);
        return false;
    }

    let requested = unsafe { SetForegroundWindow(target).as_bool() };
    let mut restored = requested || unsafe { GetForegroundWindow() == target };

    if !restored {
        unsafe {
            let current_thread = GetCurrentThreadId();
            let foreground = GetForegroundWindow();
            let foreground_thread = GetWindowThreadProcessId(foreground, None);
            let target_thread = GetWindowThreadProcessId(target, None);
            let attached_foreground = foreground_thread != 0
                && foreground_thread != current_thread
                && AttachThreadInput(current_thread, foreground_thread, true).as_bool();
            let attached_target = target_thread != 0
                && target_thread != current_thread
                && AttachThreadInput(current_thread, target_thread, true).as_bool();

            let _ = BringWindowToTop(target);
            let fallback_requested = SetForegroundWindow(target).as_bool();
            restored = fallback_requested || GetForegroundWindow() == target;

            if attached_target {
                let _ = AttachThreadInput(current_thread, target_thread, false);
            }
            if attached_foreground {
                let _ = AttachThreadInput(current_thread, foreground_thread, false);
            }
        }
    }
    log::info!(
        "FOCUS: restore target {:?}, requested={}, foreground_matches={}",
        target,
        requested,
        restored
    );
    restored
}

#[cfg(target_os = "windows")]
pub fn send_paste_input(strategy: PasteStrategy) -> u32 {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, KEYEVENTF_KEYUP, VK_CONTROL, VK_V,
    };

    let label = match strategy {
        PasteStrategy::Standard => "Ctrl+V",
        PasteStrategy::RemoteSession => "Ctrl+V after remote clipboard synchronization",
        PasteStrategy::NinjaRemote => return send_ninja_paste_as_keystrokes(),
    };
    log::info!("send_paste_input: sending {label}");
    unsafe {
        let inputs = [
            keyboard_input(VK_CONTROL, Default::default()),
            keyboard_input(VK_V, Default::default()),
            keyboard_input(VK_V, KEYEVENTF_KEYUP),
            keyboard_input(VK_CONTROL, KEYEVENTF_KEYUP),
        ];
        let result = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
        log::info!("send_paste_input: SendInput returned {}", result);
        result
    }
}

#[cfg(target_os = "windows")]
fn send_ninja_paste_as_keystrokes() -> u32 {
    use windows::Win32::Foundation::{HWND, POINT};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_MULTITHREADED,
    };
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationInvokePattern, TreeScope_Subtree,
        UIA_InvokePatternId,
    };
    use windows::Win32::UI::WindowsAndMessaging::IsWindow;

    const PASTE_BUTTON_ID: &str =
        "QApplication.NCPlayWidget.QSplitter.QWidget.NCToolBar.panel.centerFrame.PasteAsKeystrokesButton";
    const CENTER_FRAME_SUFFIX: &str = ".NCToolBar.panel.centerFrame";

    let value = PREVIOUS_FOREGROUND_WINDOW.load(Ordering::SeqCst);
    if value == 0 {
        return 0;
    }
    let target = HWND(value as _);
    if !unsafe { IsWindow(Some(target)).as_bool() } {
        return 0;
    }

    let result = unsafe {
        let initialized = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
        let invoke_result = (|| -> windows::core::Result<()> {
            let automation: IUIAutomation =
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)?;
            let root = automation.ElementFromHandle(target)?;
            let condition = automation.CreateTrueCondition()?;
            let elements = root.FindAll(TreeScope_Subtree, &condition)?;
            let mut center_frame = None;
            for index in 0..elements.Length()? {
                let element = elements.GetElement(index)?;
                if element
                    .CurrentAutomationId()?
                    .to_string()
                    .ends_with(CENTER_FRAME_SUFFIX)
                {
                    center_frame = Some(element.CurrentBoundingRectangle()?);
                    break;
                }
            }

            let frame = center_frame.ok_or_else(|| {
                windows::core::Error::new(
                    windows::core::HRESULT(0x80004005_u32 as i32),
                    "Ninja toolbar center frame was not found",
                )
            })?;
            let y = frame.top + (frame.bottom - frame.top) / 2;
            for x in (frame.left..frame.right).step_by(4) {
                let element = automation.ElementFromPoint(POINT { x, y })?;
                if element.CurrentAutomationId()? == PASTE_BUTTON_ID {
                    let pattern: IUIAutomationInvokePattern =
                        element.GetCurrentPatternAs(UIA_InvokePatternId)?;
                    return pattern.Invoke();
                }
            }

            Err(windows::core::Error::new(
                windows::core::HRESULT(0x80004005_u32 as i32),
                "Ninja Paste as Keystrokes button was not found",
            ))
        })();
        if initialized {
            CoUninitialize();
        }
        invoke_result
    };

    match result {
        Ok(()) => {
            log::info!("PASTE: invoked Ninja Paste as Keystrokes");
            1
        }
        Err(error) => {
            log::error!("PASTE: could not invoke Ninja Paste as Keystrokes: {error}");
            0
        }
    }
}

#[cfg(target_os = "windows")]
fn keyboard_input(
    key: windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY,
    flags: windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS,
) -> windows::Win32::UI::Input::KeyboardAndMouse::INPUT {
    use windows::Win32::UI::Input::KeyboardAndMouse::{INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT};

    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                dwFlags: flags,
                ..Default::default()
            },
        },
    }
}

#[cfg(not(target_os = "windows"))]
pub fn restore_previous_foreground_window() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::{
        paste_context, paste_settle_delay, paste_strategy_for_process, set_previous_target,
        should_auto_paste, should_auto_paste_with_mode, PasteStrategy,
    };

    #[test]
    fn recognizes_remote_session_clients() {
        assert_eq!(
            paste_strategy_for_process("ncplayer.exe"),
            PasteStrategy::NinjaRemote
        );
        assert_eq!(
            paste_strategy_for_process("MSTSC.EXE"),
            PasteStrategy::RemoteSession
        );
        assert_eq!(
            paste_strategy_for_process("anydesk.exe"),
            PasteStrategy::RemoteSession
        );
        assert_eq!(
            paste_strategy_for_process("notepad.exe"),
            PasteStrategy::Standard
        );
    }

    #[test]
    fn remote_targets_receive_a_longer_clipboard_sync_window() {
        assert!(
            paste_settle_delay(PasteStrategy::RemoteSession)
                > paste_settle_delay(PasteStrategy::Standard)
        );
        assert!(
            paste_settle_delay(PasteStrategy::NinjaRemote)
                < paste_settle_delay(PasteStrategy::Standard)
        );
    }

    #[test]
    fn ninja_uses_synchronized_clipboard_and_physical_paste() {
        assert!(!should_auto_paste(PasteStrategy::NinjaRemote));
        assert!(should_auto_paste(PasteStrategy::Standard));
        assert!(should_auto_paste(PasteStrategy::RemoteSession));
        assert!(!should_auto_paste_with_mode(
            PasteStrategy::NinjaRemote,
            "copy_then_paste"
        ));
        assert!(should_auto_paste_with_mode(
            PasteStrategy::NinjaRemote,
            "paste_as_keystrokes"
        ));
    }

    #[test]
    fn exposes_remote_context_for_the_flyout() {
        set_previous_target(1, PasteStrategy::NinjaRemote);

        let context = paste_context("copy_then_paste".to_string());

        assert_eq!(context.target_kind, "ninja");
        assert_eq!(context.remote_paste_mode, "copy_then_paste");
    }
}
