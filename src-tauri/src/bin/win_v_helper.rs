#[cfg(target_os = "windows")]
mod windows_helper {
    use serde::Serialize;
    use std::env;
    use std::net::UdpSocket;
    use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering};
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;
    use windows::core::PWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::Threading::{
        GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW, WaitForSingleObject, INFINITE,
        PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, MapVirtualKeyW, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT,
        KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC, VIRTUAL_KEY, VK_LCONTROL,
        VK_LMENU, VK_LWIN, VK_RCONTROL, VK_RWIN, VK_V,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetForegroundWindow, GetMessageW,
        GetWindowThreadProcessId, PostThreadMessageW, SetWindowsHookExW, TranslateMessage,
        UnhookWindowsHookEx, HC_ACTION, KBDLLHOOKSTRUCT, LLKHF_INJECTED, MSG, WH_KEYBOARD_LL,
        WM_KEYDOWN, WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP,
    };

    const DUMMY_KEY: u16 = 0x00FF;
    const CUBBY_INJECTED_FLAG: usize = 0x4355_4242;
    const REMOTE_TRIGGER_DOUBLE_TAP_MS: u32 = 400;

    static STATE: OnceLock<Mutex<RemapState>> = OnceLock::new();
    static HOOK_THREAD_ID: AtomicU32 = AtomicU32::new(0);
    static ACCEPT_TEST_INPUT: AtomicBool = AtomicBool::new(false);
    static ACTIVATION_PORT: AtomicU16 = AtomicU16::new(0);

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Decision {
        Pass,
        Suppress,
        Activate {
            win_key: u16,
            release_physical_win: bool,
        },
        RestoreWinAndPass {
            win_key: u16,
        },
        ActivateRemoteTrigger,
    }

    #[derive(Debug, Default)]
    struct RemapState {
        physical_win: Option<u16>,
        active_chord: bool,
        physical_v_down: bool,
        logical_win_released: bool,
        last_ctrl_up: Option<(u16, u32)>,
    }

    impl RemapState {
        fn handle(
            &mut self,
            key: u16,
            is_down: bool,
            is_up: bool,
            exact_match: bool,
            timestamp: u32,
        ) -> Decision {
            if key == VK_LCONTROL.0 || key == VK_RCONTROL.0 {
                if is_up {
                    let is_double_tap = self.last_ctrl_up.is_some_and(|(last_key, last_time)| {
                        last_key == key
                            && timestamp.wrapping_sub(last_time) <= REMOTE_TRIGGER_DOUBLE_TAP_MS
                    });
                    self.last_ctrl_up = (!is_double_tap).then_some((key, timestamp));
                    if is_double_tap {
                        return Decision::ActivateRemoteTrigger;
                    }
                }
                return Decision::Pass;
            }

            if is_down {
                self.last_ctrl_up = None;
            }

            if key == VK_LWIN.0 || key == VK_RWIN.0 {
                if is_down {
                    self.physical_win = Some(key);
                    return Decision::Pass;
                }

                if is_up {
                    self.physical_win = None;
                    self.physical_v_down = false;
                    self.active_chord = false;
                    if self.logical_win_released {
                        self.logical_win_released = false;
                        return Decision::Suppress;
                    }
                }
                return Decision::Pass;
            }

            if key == VK_V.0 {
                if is_down {
                    if let Some(win_key) = self.physical_win {
                        if exact_match {
                            if self.physical_v_down {
                                return Decision::Suppress;
                            }

                            self.physical_v_down = true;
                            self.active_chord = true;
                            let release_physical_win = !self.logical_win_released;
                            self.logical_win_released = true;
                            return Decision::Activate {
                                win_key,
                                release_physical_win,
                            };
                        }
                    }
                } else if is_up && self.active_chord {
                    self.physical_v_down = false;
                    return Decision::Suppress;
                }
                return Decision::Pass;
            }

            if is_down && self.logical_win_released {
                let win_key = self.physical_win.unwrap_or(VK_LWIN.0);
                self.active_chord = false;
                self.physical_v_down = false;
                self.logical_win_released = false;
                return Decision::RestoreWinAndPass { win_key };
            }

            Decision::Pass
        }

        fn activation_failed(&mut self, release_was_attempted: bool) {
            self.active_chord = false;
            self.physical_v_down = false;
            self.logical_win_released = release_was_attempted;
        }
    }

    #[derive(Serialize)]
    struct OutputEvent<'a> {
        event: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        timeout_seconds: Option<u64>,
    }

    fn emit(event: &str, detail: Option<&str>) {
        println!(
            "{}",
            serde_json::to_string(&OutputEvent {
                event,
                detail,
                timeout_seconds: None,
            })
            .expect("serialize helper event")
        );
    }

    fn key_input(key: u16, key_up: bool) -> INPUT {
        let mut flags = if key_up {
            KEYEVENTF_KEYUP
        } else {
            Default::default()
        };
        if matches!(
            key,
            0x21..=0x2E
                | 0x5B..=0x5D
                | 0x6F
                | 0x90
                | 0xA3
                | 0xA5
                | 0xA6..=0xAC
                | 0xAD..=0xB7
        ) {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }

        INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: VIRTUAL_KEY(key),
                    wScan: unsafe { MapVirtualKeyW(key as u32, MAPVK_VK_TO_VSC) as u16 },
                    dwFlags: flags,
                    dwExtraInfo: CUBBY_INJECTED_FLAG,
                    ..Default::default()
                },
            },
        }
    }

    fn target_shortcut_inputs() -> Vec<INPUT> {
        vec![
            key_input(VK_LWIN.0, false),
            key_input(VK_LMENU.0, false),
            key_input(VK_V.0, false),
            key_input(VK_V.0, true),
            key_input(VK_LMENU.0, true),
            key_input(VK_LWIN.0, true),
        ]
    }

    fn release_win_inputs(win_key: u16) -> [INPUT; 3] {
        [
            key_input(DUMMY_KEY, false),
            key_input(DUMMY_KEY, true),
            key_input(win_key, true),
        ]
    }

    fn recovery_keyups() {
        let inputs = [
            key_input(VK_V.0, true),
            key_input(VK_LMENU.0, true),
            key_input(VK_LWIN.0, true),
            key_input(VK_RWIN.0, true),
        ];
        unsafe {
            let _ = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
        }
    }

    #[derive(Debug, Clone, Copy)]
    enum InjectionError {
        Blocked,
        Partial,
    }

    fn inject_all(inputs: &[INPUT]) -> Result<(), InjectionError> {
        let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) } as usize;
        if sent == inputs.len() {
            Ok(())
        } else if sent == 0 {
            Err(InjectionError::Blocked)
        } else {
            recovery_keyups();
            Err(InjectionError::Partial)
        }
    }

    fn notify_cubby() -> Result<(), String> {
        let activation_port = ACTIVATION_PORT.load(Ordering::SeqCst);
        if activation_port == 0 {
            return inject_all(&target_shortcut_inputs())
                .map_err(|error| format!("shortcut fallback injection failed: {error:?}"));
        }

        let socket = UdpSocket::bind(("127.0.0.1", 0))
            .map_err(|error| format!("could not create activation socket: {error}"))?;
        let sent = socket
            .send_to(b"activate", ("127.0.0.1", activation_port))
            .map_err(|error| format!("could not signal Cubby: {error}"))?;
        if sent != b"activate".len() {
            return Err(format!("activation message was truncated to {sent} bytes"));
        }
        Ok(())
    }

    fn activate_cubby(win_key: Option<u16>, release_physical_win: bool) -> Result<(), String> {
        if release_physical_win {
            let win_key = win_key.unwrap_or(VK_LWIN.0);
            inject_all(&release_win_inputs(win_key))
                .map_err(|error| format!("Windows key release failed: {error:?}"))?;
        }
        notify_cubby()
    }

    fn keyboard_is_exact_win_v() -> bool {
        for key in 8..=255i32 {
            if key == VK_LWIN.0 as i32 || key == VK_RWIN.0 as i32 || key == VK_V.0 as i32 {
                continue;
            }
            if unsafe { GetAsyncKeyState(key) } < 0 {
                return false;
            }
        }
        true
    }

    fn is_supported_remote_process(process_name: &str) -> bool {
        matches!(
            process_name.to_ascii_lowercase().as_str(),
            "ncplayer.exe"
                | "mstsc.exe"
                | "msrdc.exe"
                | "anydesk.exe"
                | "teamviewer.exe"
                | "teamviewer_desktop.exe"
                | "screenconnect.clientservice.exe"
                | "screenconnect.windowsclient.exe"
                | "splashtop.exe"
                | "strwinclt.exe"
                | "rustdesk.exe"
        )
    }

    fn foreground_is_supported_remote() -> bool {
        unsafe {
            let foreground = GetForegroundWindow();
            if foreground.0.is_null() {
                return false;
            }
            let mut process_id = 0;
            GetWindowThreadProcessId(foreground, Some(&mut process_id));
            if process_id == 0 {
                return false;
            }

            let Ok(process) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, process_id)
            else {
                return false;
            };
            let mut buffer = vec![0_u16; 1024];
            let mut length = buffer.len() as u32;
            let result = QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_WIN32,
                PWSTR(buffer.as_mut_ptr()),
                &mut length,
            );
            let _ = CloseHandle(process);
            if result.is_err() {
                return false;
            }

            let path = String::from_utf16_lossy(&buffer[..length as usize]);
            std::path::Path::new(&path)
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(is_supported_remote_process)
        }
    }

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code != HC_ACTION as i32 {
            return unsafe { CallNextHookEx(None, code, wparam, lparam) };
        }

        let event = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
        if ACCEPT_TEST_INPUT.load(Ordering::SeqCst)
            && (event.vkCode == VK_LWIN.0 as u32
                || event.vkCode == VK_RWIN.0 as u32
                || event.vkCode == VK_LCONTROL.0 as u32
                || event.vkCode == VK_RCONTROL.0 as u32
                || event.vkCode == VK_V.0 as u32)
        {
            emit(
                "test_key",
                Some(if event.flags.0 & LLKHF_INJECTED.0 != 0 {
                    "injected"
                } else {
                    "physical"
                }),
            );
        }
        if event.dwExtraInfo == CUBBY_INJECTED_FLAG
            || (event.flags.0 & LLKHF_INJECTED.0 != 0 && !ACCEPT_TEST_INPUT.load(Ordering::SeqCst))
        {
            return unsafe { CallNextHookEx(None, code, wparam, lparam) };
        }

        let message = wparam.0 as u32;
        let is_down = message == WM_KEYDOWN || message == WM_SYSKEYDOWN;
        let is_up = message == WM_KEYUP || message == WM_SYSKEYUP;
        let key = event.vkCode as u16;
        let exact_match = if key == VK_V.0 && is_down {
            keyboard_is_exact_win_v()
        } else {
            true
        };

        let decision = STATE
            .get_or_init(|| Mutex::new(RemapState::default()))
            .lock()
            .expect("Win+V state mutex poisoned")
            .handle(key, is_down, is_up, exact_match, event.time);

        match decision {
            Decision::Pass => unsafe { CallNextHookEx(None, code, wparam, lparam) },
            Decision::Suppress => LRESULT(1),
            Decision::RestoreWinAndPass { win_key } => {
                let restored = inject_all(&[key_input(win_key, false)]).is_ok();
                if restored && ACCEPT_TEST_INPUT.load(Ordering::SeqCst) {
                    emit("win_restored", Some("physical Win state restored"));
                } else if !restored {
                    emit(
                        "restore_failed",
                        Some("physical Win state could not be restored"),
                    );
                }
                unsafe { CallNextHookEx(None, code, wparam, lparam) }
            }
            Decision::Activate {
                win_key,
                release_physical_win,
            } => match activate_cubby(Some(win_key), release_physical_win) {
                Ok(()) => {
                    emit("win_v", Some("activated Cubby directly"));
                    LRESULT(1)
                }
                Err(error) => {
                    if let Some(state) = STATE.get() {
                        state
                            .lock()
                            .expect("Win+V state mutex poisoned")
                            .activation_failed(release_physical_win);
                    }
                    emit("activation_failed", Some(&error));
                    if release_physical_win {
                        LRESULT(1)
                    } else {
                        unsafe { CallNextHookEx(None, code, wparam, lparam) }
                    }
                }
            },
            Decision::ActivateRemoteTrigger => {
                if ACCEPT_TEST_INPUT.load(Ordering::SeqCst) || foreground_is_supported_remote() {
                    match activate_cubby(None, false) {
                        Ok(()) => emit("remote_trigger", Some("activated Cubby directly")),
                        Err(error) => emit("activation_failed", Some(&error)),
                    }
                } else {
                    emit(
                        "remote_trigger_ignored",
                        Some("foreground app is not a supported remote client"),
                    );
                }
                unsafe { CallNextHookEx(None, code, wparam, lparam) }
            }
        }
    }

    fn parse_timeout_seconds() -> u64 {
        let args: Vec<String> = env::args().collect();
        args.windows(2)
            .find(|pair| pair[0] == "--timeout-seconds")
            .and_then(|pair| pair[1].parse::<u64>().ok())
            .unwrap_or(300)
    }

    fn parse_parent_pid() -> Option<u32> {
        let args: Vec<String> = env::args().collect();
        args.windows(2)
            .find(|pair| pair[0] == "--parent-pid")
            .and_then(|pair| pair[1].parse::<u32>().ok())
    }

    fn parse_activation_port() -> Option<u16> {
        let args: Vec<String> = env::args().collect();
        args.windows(2)
            .find(|pair| pair[0] == "--activation-port")
            .and_then(|pair| pair[1].parse::<u16>().ok())
    }

    fn stop_when_parent_exits(parent_pid: u32) -> Result<(), String> {
        let parent = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, parent_pid) }
            .map_err(|error| format!("failed to monitor Cubby process {parent_pid}: {error}"))?;
        let parent_handle = parent.0 as usize;

        std::thread::spawn(move || {
            let parent = HANDLE(parent_handle as *mut std::ffi::c_void);
            unsafe {
                WaitForSingleObject(parent, INFINITE);
                let _ = CloseHandle(parent);
            }
            let thread_id = HOOK_THREAD_ID.load(Ordering::SeqCst);
            if thread_id != 0 {
                unsafe {
                    let _ = PostThreadMessageW(thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
                }
            }
        });
        Ok(())
    }

    pub fn run() -> Result<(), String> {
        let timeout_seconds = parse_timeout_seconds();
        let parent_pid = parse_parent_pid();
        ACTIVATION_PORT.store(parse_activation_port().unwrap_or(0), Ordering::SeqCst);
        ACCEPT_TEST_INPUT.store(
            env::args().any(|arg| arg == "--accept-injected-test-events"),
            Ordering::SeqCst,
        );
        STATE.get_or_init(|| Mutex::new(RemapState::default()));

        let thread_id = unsafe { GetCurrentThreadId() };
        HOOK_THREAD_ID.store(thread_id, Ordering::SeqCst);
        if let Some(parent_pid) = parent_pid {
            stop_when_parent_exits(parent_pid)?;
        } else {
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(timeout_seconds));
                let thread_id = HOOK_THREAD_ID.load(Ordering::SeqCst);
                if thread_id != 0 {
                    unsafe {
                        let _ = PostThreadMessageW(thread_id, WM_QUIT, WPARAM(0), LPARAM(0));
                    }
                }
            });
        }

        let hook = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0) }
            .map_err(|error| format!("failed to install Win+V hook: {error}"))?;

        println!(
            "{}",
            serde_json::to_string(&OutputEvent {
                event: "ready",
                detail: Some("Win+V and double Ctrl activate Cubby"),
                timeout_seconds: parent_pid.is_none().then_some(timeout_seconds),
            })
            .map_err(|error| error.to_string())?
        );

        unsafe {
            let mut message = MSG::default();
            while GetMessageW(&mut message, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            let _ = UnhookWindowsHookEx(hook);
        }
        emit("stopped", None);
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::{is_supported_remote_process, Decision, RemapState};
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            VK_E, VK_LCONTROL, VK_LWIN, VK_RCONTROL, VK_V,
        };

        #[test]
        fn exact_win_v_chord_is_fully_suppressed() {
            let mut state = RemapState::default();
            assert_eq!(
                state.handle(VK_LWIN.0, true, false, true, 10),
                Decision::Pass
            );
            assert_eq!(
                state.handle(VK_V.0, true, false, true, 20),
                Decision::Activate {
                    win_key: VK_LWIN.0,
                    release_physical_win: true
                }
            );
            assert_eq!(
                state.handle(VK_V.0, false, true, true, 30),
                Decision::Suppress
            );
            assert_eq!(
                state.handle(VK_LWIN.0, false, true, true, 40),
                Decision::Suppress
            );
        }

        #[test]
        fn ordinary_v_is_never_suppressed() {
            let mut state = RemapState::default();
            assert_eq!(state.handle(VK_V.0, true, false, true, 10), Decision::Pass);
            assert_eq!(state.handle(VK_V.0, false, true, true, 20), Decision::Pass);
        }

        #[test]
        fn repeated_v_while_holding_win_retriggers() {
            let mut state = RemapState::default();
            state.handle(VK_LWIN.0, true, false, true, 10);
            state.handle(VK_V.0, true, false, true, 20);
            state.handle(VK_V.0, false, true, true, 30);
            assert_eq!(
                state.handle(VK_V.0, true, false, true, 40),
                Decision::Activate {
                    win_key: VK_LWIN.0,
                    release_physical_win: false
                }
            );
        }

        #[test]
        fn unrelated_win_shortcut_restores_win_state() {
            let mut state = RemapState::default();
            state.handle(VK_LWIN.0, true, false, true, 10);
            state.handle(VK_V.0, true, false, true, 20);
            state.handle(VK_V.0, false, true, true, 30);
            assert_eq!(
                state.handle(VK_E.0, true, false, true, 40),
                Decision::RestoreWinAndPass { win_key: VK_LWIN.0 }
            );
            assert_eq!(
                state.handle(VK_LWIN.0, false, true, true, 50),
                Decision::Pass
            );
        }

        #[test]
        fn non_exact_win_v_is_left_to_windows() {
            let mut state = RemapState::default();
            state.handle(VK_LWIN.0, true, false, true, 10);
            assert_eq!(state.handle(VK_V.0, true, false, false, 20), Decision::Pass);
        }

        #[test]
        fn double_tapping_right_ctrl_activates_remote_trigger() {
            let mut state = RemapState::default();
            assert_eq!(
                state.handle(VK_RCONTROL.0, false, true, true, 100),
                Decision::Pass
            );
            assert_eq!(
                state.handle(VK_RCONTROL.0, false, true, true, 420),
                Decision::ActivateRemoteTrigger
            );
        }

        #[test]
        fn double_tapping_left_ctrl_activates_remote_trigger() {
            let mut state = RemapState::default();
            assert_eq!(
                state.handle(VK_LCONTROL.0, false, true, true, 100),
                Decision::Pass
            );
            assert_eq!(
                state.handle(VK_LCONTROL.0, false, true, true, 420),
                Decision::ActivateRemoteTrigger
            );
        }

        #[test]
        fn alternating_ctrl_keys_do_not_activate() {
            let mut state = RemapState::default();
            state.handle(VK_LCONTROL.0, false, true, true, 100);
            assert_eq!(
                state.handle(VK_RCONTROL.0, false, true, true, 200),
                Decision::Pass
            );
        }

        #[test]
        fn recognizes_supported_remote_clients() {
            assert!(is_supported_remote_process("ncplayer.exe"));
            assert!(is_supported_remote_process("MSTSC.EXE"));
            assert!(is_supported_remote_process("rustdesk.exe"));
            assert!(!is_supported_remote_process("notepad.exe"));
        }

        #[test]
        fn slow_right_ctrl_taps_do_not_activate() {
            let mut state = RemapState::default();
            state.handle(VK_RCONTROL.0, false, true, true, 100);
            assert_eq!(
                state.handle(VK_RCONTROL.0, false, true, true, 501),
                Decision::Pass
            );
        }
    }
}

#[cfg(target_os = "windows")]
pub fn run_embedded() -> Result<(), String> {
    windows_helper::run()
}

#[cfg(not(target_os = "windows"))]
pub fn run_embedded() -> Result<(), String> {
    Err("win_v_helper is only supported on Windows".to_string())
}

#[cfg(target_os = "windows")]
#[allow(dead_code)]
fn main() {
    if let Err(error) = run_embedded() {
        eprintln!("{{\"event\":\"error\",\"detail\":{error:?}}}");
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "windows"))]
#[allow(dead_code)]
fn main() {
    eprintln!("win_v_helper is only supported on Windows");
    std::process::exit(1);
}
