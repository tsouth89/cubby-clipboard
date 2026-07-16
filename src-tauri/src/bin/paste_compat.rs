#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("paste_compat only runs on Windows");
    std::process::exit(1);
}

#[cfg(target_os = "windows")]
fn main() {
    use clipboard_rs::{Clipboard, ClipboardContext};
    use std::thread;
    use std::time::Duration;
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::HINSTANCE;
    use windows::Win32::Graphics::Gdi::UpdateWindow;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, RegisterClassW, SetForegroundWindow, SetWindowTextW,
        ShowWindow, CS_HREDRAW, CS_VREDRAW, CW_USEDEFAULT, ES_AUTOHSCROLL, ES_AUTOVSCROLL,
        ES_MULTILINE, SW_HIDE, SW_SHOW, WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSW, WS_CHILD,
        WS_OVERLAPPEDWINDOW, WS_VISIBLE,
    };

    let cases = [
        "simple text",
        "  leading and trailing whitespace  ",
        "line one\r\nline two\r\nline three",
        "Unicode: café — Ελληνικά — 日本語 — 😀",
        "https://cubbyclipboard.com/path?q=clipboard&mode=paste",
        "const cubby = { reliable: true };",
    ];

    unsafe {
        let module = GetModuleHandleW(None).expect("module");
        let instance = HINSTANCE(module.0);
        let class_name = w!("CubbyPasteCompatibilityTarget");
        RegisterClassW(&WNDCLASSW {
            style: CS_HREDRAW | CS_VREDRAW,
            hInstance: instance,
            lpszClassName: class_name,
            lpfnWndProc: Some(window_proc),
            ..Default::default()
        });

        let target = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            w!("Cubby Paste Target"),
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            600,
            300,
            None,
            None,
            Some(instance),
            None,
        )
        .expect("target window");
        let edit = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("EDIT"),
            PCWSTR::null(),
            WS_CHILD
                | WS_VISIBLE
                | WINDOW_STYLE(ES_MULTILINE as u32)
                | WINDOW_STYLE(ES_AUTOVSCROLL as u32)
                | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
            10,
            10,
            560,
            240,
            Some(target),
            None,
            Some(instance),
            None,
        )
        .expect("edit control");
        let cubby = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class_name,
            w!("Cubby Simulation"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            300,
            200,
            None,
            None,
            Some(instance),
            None,
        )
        .expect("cubby simulation");

        let _ = ShowWindow(target, SW_SHOW);
        UpdateWindow(target).expect("update target");
        pump_messages();
        let clipboard = ClipboardContext::new().expect("clipboard");
        let mut passed = 0;

        for iteration in 0..30 {
            let expected = cases[iteration % cases.len()];
            SetWindowTextW(edit, w!("")).expect("clear edit");
            clipboard
                .set_text(expected.to_string())
                .expect("set clipboard");

            let _ = SetForegroundWindow(target);
            SetFocus(Some(edit)).expect("focus edit");
            cubby::paste_engine::set_previous_foreground_window(target.0 as isize);
            let _ = ShowWindow(cubby, SW_SHOW);
            let _ = SetForegroundWindow(cubby);
            pump_messages();
            let _ = ShowWindow(cubby, SW_HIDE);
            pump_messages();

            assert!(
                cubby::paste_engine::restore_previous_foreground_window(),
                "iteration {iteration}: target focus was not restored"
            );
            SetFocus(Some(edit)).expect("restore edit focus");
            thread::sleep(Duration::from_millis(25));
            assert_eq!(
                cubby::paste_engine::send_paste_input(),
                4,
                "iteration {iteration}: SendInput did not accept all events"
            );

            for _ in 0..20 {
                pump_messages();
                thread::sleep(Duration::from_millis(5));
            }

            let actual = window_text(edit);
            assert_eq!(
                actual, expected,
                "iteration {iteration}: pasted text differs"
            );
            passed += 1;
        }

        cubby::paste_engine::set_previous_foreground_window(isize::MAX);
        assert!(
            !cubby::paste_engine::restore_previous_foreground_window(),
            "invalid target should fail safely"
        );

        DestroyWindow(cubby).expect("destroy cubby");
        DestroyWindow(target).expect("destroy target");
        println!(
            "{}",
            serde_json::json!({
                "event": "summary",
                "passed": true,
                "paste_cases": passed,
                "invalid_target_safe": true
            })
        );
    }
}

#[cfg(target_os = "windows")]
unsafe fn pump_messages() {
    use windows::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
    };
    let mut message = MSG::default();
    while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
        let _ = TranslateMessage(&message);
        DispatchMessageW(&message);
    }
}

#[cfg(target_os = "windows")]
unsafe fn window_text(window: windows::Win32::Foundation::HWND) -> String {
    use windows::Win32::UI::WindowsAndMessaging::{GetWindowTextLengthW, GetWindowTextW};
    let length = GetWindowTextLengthW(window);
    let mut buffer = vec![0_u16; length as usize + 1];
    let read = GetWindowTextW(window, &mut buffer);
    String::from_utf16_lossy(&buffer[..read as usize])
}

#[cfg(target_os = "windows")]
unsafe extern "system" fn window_proc(
    hwnd: windows::Win32::Foundation::HWND,
    message: u32,
    wparam: windows::Win32::Foundation::WPARAM,
    lparam: windows::Win32::Foundation::LPARAM,
) -> windows::Win32::Foundation::LRESULT {
    windows::Win32::UI::WindowsAndMessaging::DefWindowProcW(hwnd, message, wparam, lparam)
}
