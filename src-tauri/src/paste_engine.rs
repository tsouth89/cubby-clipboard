#[cfg(target_os = "windows")]
use std::sync::atomic::{AtomicIsize, Ordering};

#[cfg(target_os = "windows")]
static PREVIOUS_FOREGROUND_WINDOW: AtomicIsize = AtomicIsize::new(0);

#[cfg(target_os = "windows")]
pub fn remember_foreground_window(excluded_hwnd: Option<isize>) -> Option<isize> {
    use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

    let foreground = unsafe { GetForegroundWindow() };
    let value = foreground.0 as isize;
    if value == 0 || Some(value) == excluded_hwnd {
        return None;
    }

    PREVIOUS_FOREGROUND_WINDOW.store(value, Ordering::SeqCst);
    Some(value)
}

#[cfg(target_os = "windows")]
pub fn set_previous_foreground_window(hwnd: isize) {
    PREVIOUS_FOREGROUND_WINDOW.store(hwnd, Ordering::SeqCst);
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
pub fn send_paste_input() -> u32 {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, KEYEVENTF_KEYUP, VK_CONTROL, VK_V,
    };

    log::info!("send_paste_input: sending Ctrl+V");
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
