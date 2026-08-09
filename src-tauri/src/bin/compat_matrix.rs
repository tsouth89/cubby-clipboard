//! Clipboard application-compatibility matrix (SBS-408).
//!
//! Drives real Windows applications through capture -> restore -> paste and
//! reports every failure with the app, format, and step that produced it. Rows
//! whose application is not installed, or whose environment does not apply, are
//! reported as explicit skips with a reason -- never dropped silently, so a
//! green run can always be told apart from a run that proved nothing.
//!
//! The verdict logic lives in `cubby::compat_matrix_model` so it can be unit
//! tested without a desktop; this file is the driver.

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("compat_matrix only runs on Windows");
    std::process::exit(1);
}

#[cfg(target_os = "windows")]
#[path = "../clipboard_policy.rs"]
mod clipboard_policy;

#[cfg(target_os = "windows")]
fn main() {
    windows_matrix::run();
}

#[cfg(target_os = "windows")]
mod windows_matrix {
    use crate::clipboard_policy::{classify_file_payload, FilePayloadPolicy};
    use clipboard_rs::{Clipboard, ClipboardContent, ClipboardContext};
    use cubby::compat_matrix_model::{
        class_selected, parse_class_filter, summarize, AppClass, Failure, Format, MatrixSummary,
        RowOutcome, RowResult, SkipReason, Step,
    };
    use cubby::paste_engine::{
        paste_settle_delay, restore_previous_foreground_window, send_paste_input,
        set_previous_target, PasteStrategy,
    };
    use std::env;
    use std::path::{Path, PathBuf};
    use std::process::{Child, Command};
    use std::thread;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, LPARAM, TRUE};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
    };
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationTextPattern,
        IUIAutomationValuePattern, TreeScope_Descendants, UIA_ControlTypePropertyId,
        UIA_DocumentControlTypeId, UIA_EditControlTypeId, UIA_TextPatternId, UIA_ValuePatternId,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
        IsWindowVisible,
    };

    /// Text payload used for every row that can round-trip a string. It mixes
    /// scripts, an emoji, and interior punctuation so a lossy transcoder shows
    /// up as a mismatch rather than passing by luck. Single line, because the
    /// console row reads exactly one line.
    const TEXT_PAYLOAD: &str = "Cubby SBS-408 — café · Ελληνικά · 日本語 · 😀 · end";

    const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);

    /// Office and Electron editors take noticeably longer than a Notepad to go
    /// from process start to a usable document.
    const APP_LAUNCH_TIMEOUT: Duration = Duration::from_secs(60);

    /// How many times a row re-tries the activate/focus/paste sequence before
    /// calling it a compatibility failure.
    const PASTE_ATTEMPTS: u32 = 3;

    /// Per-attempt budget for the pasted text to show up in the target.
    const PASTE_READBACK_TIMEOUT: Duration = Duration::from_secs(6);

    struct Options {
        filter: Vec<AppClass>,
        list_only: bool,
        keep_apps: bool,
    }

    pub fn run() {
        let options = match parse_args() {
            Ok(options) => options,
            Err(error) => {
                eprintln!("{error}");
                std::process::exit(2);
            }
        };

        if options.list_only {
            for class in AppClass::ALL {
                println!("{}", class.slug());
            }
            return;
        }

        // UI Automation and the shell paste both need COM on this thread.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }

        let mut results = Vec::new();
        for class in AppClass::ALL {
            if !class_selected(class, &options.filter) {
                results.push(RowResult {
                    class,
                    app: "n/a".to_string(),
                    formats: Vec::new(),
                    outcome: RowOutcome::Skipped {
                        reason: SkipReason::Deselected,
                    },
                });
                continue;
            }

            let result = run_class(class, &options);
            emit_row(&result);
            results.push(result);
        }

        let summary = summarize(&results);
        emit_summary(&summary);
        if !summary.passed {
            std::process::exit(1);
        }
    }

    fn parse_args() -> Result<Options, String> {
        let mut filter = Vec::new();
        let mut list_only = false;
        let mut keep_apps = false;
        let mut args = env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--only" => {
                    let raw = args.next().ok_or_else(|| {
                        "--only requires a comma-separated class list".to_string()
                    })?;
                    filter = parse_class_filter(&raw)?;
                }
                "--list" => list_only = true,
                "--keep-apps" => keep_apps = true,
                other => return Err(format!("unknown argument '{other}'")),
            }
        }
        Ok(Options {
            filter,
            list_only,
            keep_apps,
        })
    }

    fn emit_row(result: &RowResult) {
        let (status, detail) = match &result.outcome {
            RowOutcome::Passed { checks } => ("passed", format!("{checks} checks")),
            RowOutcome::Failed { failures } => (
                "failed",
                failures
                    .iter()
                    .map(Failure::to_string)
                    .collect::<Vec<_>>()
                    .join("; "),
            ),
            RowOutcome::Skipped { reason } => ("skipped", reason.detail()),
        };
        println!(
            "{}",
            serde_json::json!({
                "event": "row",
                "class": result.class.slug(),
                "app": result.app,
                "formats": result.formats.iter().map(|format| format.slug()).collect::<Vec<_>>(),
                "status": status,
                "detail": detail,
            })
        );
    }

    fn emit_summary(summary: &MatrixSummary) {
        println!(
            "{}",
            serde_json::json!({
                "event": "summary",
                "passed": summary.passed,
                "rows_total": summary.rows_total,
                "rows_passed": summary.rows_passed,
                "rows_failed": summary.rows_failed,
                "rows_skipped": summary.rows_skipped,
                "checks": summary.checks,
                "failures": summary.failures.iter().map(|failure| serde_json::json!({
                    "class": failure.class.slug(),
                    "app": failure.app,
                    "format": failure.format.slug(),
                    "step": failure.step.slug(),
                    "detail": failure.detail,
                })).collect::<Vec<_>>(),
                "classes_not_covered": summary
                    .classes_not_covered
                    .iter()
                    .map(|class| class.slug())
                    .collect::<Vec<_>>(),
            })
        );
    }

    fn run_class(class: AppClass, options: &Options) -> RowResult {
        match class {
            AppClass::LegacyWin32 => run_owned_edit_row(class, PasteStrategy::Standard),
            AppClass::RemoteSession => run_owned_edit_row(class, PasteStrategy::RemoteSession),
            AppClass::PackagedApp => run_uia_app_row(
                class,
                &[AppCandidate::new("notepad.exe", &["notepad.exe"], &[])],
                options,
            ),
            AppClass::Browser => run_browser_row(class, options),
            AppClass::Ide => run_uia_app_row(
                class,
                &[
                    // Electron exposes a UIA tree only when renderer
                    // accessibility is on; without the flag VS Code has no
                    // editable element to find.
                    AppCandidate::with_document("code.exe", &["code.exe"], &[], "txt"),
                    AppCandidate::with_document("notepad++.exe", &["notepad++.exe"], &[], "txt"),
                ],
                options,
            ),
            AppClass::Office => run_uia_app_row(
                class,
                &[
                    // Word opens on a start screen with no document when it is
                    // launched bare, so the row hands it a file to open.
                    AppCandidate::with_document("winword.exe", &["winword.exe"], &["/q"], "rtf"),
                    AppCandidate::with_document("wordpad.exe", &["wordpad.exe"], &[], "rtf"),
                ],
                options,
            ),
            AppClass::Terminal => run_terminal_row(class),
            AppClass::Explorer => run_explorer_row(class),
            AppClass::Elevated => run_elevated_row(class),
        }
    }

    // ---------------------------------------------------------------- helpers

    fn failure(class: AppClass, app: &str, format: Format, step: Step, detail: String) -> Failure {
        Failure {
            class,
            app: app.to_string(),
            format,
            step,
            detail,
        }
    }

    fn passed(class: AppClass, app: &str, formats: Vec<Format>, checks: usize) -> RowResult {
        RowResult {
            class,
            app: app.to_string(),
            formats,
            outcome: RowOutcome::Passed { checks },
        }
    }

    fn failed(
        class: AppClass,
        app: &str,
        formats: Vec<Format>,
        failures: Vec<Failure>,
    ) -> RowResult {
        RowResult {
            class,
            app: app.to_string(),
            formats,
            outcome: RowOutcome::Failed { failures },
        }
    }

    fn skipped(class: AppClass, reason: SkipReason) -> RowResult {
        RowResult {
            class,
            app: "n/a".to_string(),
            formats: Vec::new(),
            outcome: RowOutcome::Skipped { reason },
        }
    }

    fn unique_suffix() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    }

    /// Put text on the clipboard the way Cubby restores a stored clip, and read
    /// it back. This is the `restore` step: if the bytes do not survive the
    /// round trip there is no point pasting them anywhere.
    fn restore_text(clipboard: &ClipboardContext, text: &str) -> Result<(), String> {
        clipboard
            .set(vec![ClipboardContent::Text(text.to_string())])
            .map_err(|error| error.to_string())?;
        // The clipboard owner publishes asynchronously; give it a moment before
        // reading back rather than racing it.
        thread::sleep(Duration::from_millis(120));
        let observed = clipboard.get_text().map_err(|error| error.to_string())?;
        if observed != text {
            return Err(format!(
                "clipboard round trip differed (wrote {} chars, read {} chars)",
                text.chars().count(),
                observed.chars().count()
            ));
        }
        Ok(())
    }

    /// The `capture` step for text: Cubby's capture policy must treat a plain
    /// text payload as storable history rather than an ignored file payload.
    fn capture_text_is_storable() -> Result<(), String> {
        let policy = classify_file_payload(false, false);
        if policy != FilePayloadPolicy::Materialize {
            return Err(format!(
                "text payload classified as {policy:?}, expected Materialize"
            ));
        }
        Ok(())
    }

    /// Bring the target window to the foreground through the production restore
    /// path. Split from [`send_paste`] because activating a window can move
    /// focus within it: any per-control focusing has to happen after the window
    /// is already frontmost, or the activation undoes it.
    fn activate_target(hwnd: HWND, strategy: PasteStrategy) -> Result<(), String> {
        set_previous_target(hwnd.0 as isize, strategy);
        if !restore_with_activation(hwnd) {
            return Err("could not restore the target window to the foreground".to_string());
        }
        Ok(())
    }

    fn send_paste(strategy: PasteStrategy) -> Result<(), String> {
        thread::sleep(paste_settle_delay(strategy));
        let sent = send_paste_input(strategy);
        if sent != 4 {
            return Err(format!("SendInput accepted {sent} of 4 paste events"));
        }
        Ok(())
    }

    fn paste_into(hwnd: HWND, strategy: PasteStrategy) -> Result<(), String> {
        activate_target(hwnd, strategy)?;
        send_paste(strategy)
    }

    /// Windows refuses `SetForegroundWindow` from a process that does not
    /// already own the foreground, which is exactly the harness's situation once
    /// it has driven one application and moved on to the next. Production's
    /// restore is tried first so the row exercises the real code path; only when
    /// the foreground lock refuses it does the harness fall back to a
    /// minimize/restore cycle, which reactivates the window and grants the
    /// foreground rights the next attempt needs.
    fn restore_with_activation(hwnd: HWND) -> bool {
        use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_MINIMIZE, SW_RESTORE};

        for attempt in 0..4 {
            if restore_previous_foreground_window() {
                return true;
            }
            unsafe {
                let _ = ShowWindow(hwnd, SW_MINIMIZE);
                thread::sleep(Duration::from_millis(80));
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
            thread::sleep(Duration::from_millis(150 * (attempt + 1)));
        }
        restore_previous_foreground_window()
    }

    /// Poll a read-back until it matches or the deadline passes. Applications
    /// apply a paste asynchronously, so a single immediate read is a race.
    fn wait_for<T, F>(timeout: Duration, mut probe: F) -> Result<T, String>
    where
        F: FnMut() -> Result<Option<T>, String>,
    {
        let deadline = Instant::now() + timeout;
        let mut last_error = None;
        loop {
            match probe() {
                Ok(Some(value)) => return Ok(value),
                Ok(None) => {}
                Err(error) => last_error = Some(error),
            }
            if Instant::now() >= deadline {
                return Err(
                    last_error.unwrap_or_else(|| "timed out waiting for the target".to_string())
                );
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn find_executable(name: &str) -> Option<PathBuf> {
        if let Ok(path) = env::var("PATH") {
            for directory in env::split_paths(&path) {
                let candidate = directory.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }

        // Apps that are not on PATH but live in predictable install roots.
        let roots = [
            env::var("ProgramFiles").ok(),
            env::var("ProgramFiles(x86)").ok(),
            env::var("LOCALAPPDATA")
                .ok()
                .map(|base| format!("{base}\\Programs")),
        ];
        for root in roots.into_iter().flatten() {
            let root = PathBuf::from(root);
            if let Some(found) = search_shallow(&root, name, 3) {
                return Some(found);
            }
        }
        None
    }

    /// Bounded-depth search: install roots are wide, and an unbounded walk of
    /// Program Files costs seconds per lookup.
    fn search_shallow(directory: &Path, name: &str, depth: usize) -> Option<PathBuf> {
        let candidate = directory.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        if depth == 0 {
            return None;
        }
        let entries = std::fs::read_dir(directory).ok()?;
        for entry in entries.flatten() {
            if entry.file_type().ok()?.is_dir() {
                if let Some(found) = search_shallow(&entry.path(), name, depth - 1) {
                    return Some(found);
                }
            }
        }
        None
    }

    // ------------------------------------------------------- window discovery

    struct WindowSearch {
        pid: u32,
        /// Packaged apps (Win11 Notepad) and browsers hand the launch off to an
        /// already-running host process, so the window we want frequently
        /// belongs to a different pid than the one we spawned. Matching the
        /// executable name as well is what keeps those rows runnable.
        exe_name: String,
        found: HWND,
    }

    unsafe extern "system" fn enum_window_proc(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
        let search = &mut *(lparam.0 as *mut WindowSearch);
        if !IsWindowVisible(hwnd).as_bool() || GetWindowTextLengthW(hwnd) == 0 {
            return TRUE;
        }
        let mut pid = 0_u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        let matches = pid == search.pid
            || process_image_name(pid)
                .is_some_and(|name| name.eq_ignore_ascii_case(&search.exe_name));
        if matches {
            search.found = hwnd;
            return windows::core::BOOL(0); // stop enumerating
        }
        TRUE
    }

    fn process_image_name(pid: u32) -> Option<String> {
        use windows::Win32::System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
            PROCESS_QUERY_LIMITED_INFORMATION,
        };
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buffer = [0_u16; 512];
            let mut length = buffer.len() as u32;
            let result = QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_FORMAT(0),
                windows::core::PWSTR(buffer.as_mut_ptr()),
                &mut length,
            );
            let _ = CloseHandle(handle);
            result.ok()?;
            let path = String::from_utf16_lossy(&buffer[..length as usize]);
            Path::new(&path)
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string)
        }
    }

    fn enum_find_window(pid: u32, exe_name: &str) -> Option<HWND> {
        let mut search = WindowSearch {
            pid,
            exe_name: exe_name.to_string(),
            found: HWND::default(),
        };
        unsafe {
            let _ = EnumWindows(
                Some(enum_window_proc),
                LPARAM(&mut search as *mut WindowSearch as isize),
            );
        }
        (!search.found.0.is_null()).then_some(search.found)
    }

    fn window_text(hwnd: HWND) -> String {
        unsafe {
            let length = GetWindowTextLengthW(hwnd);
            if length <= 0 {
                return String::new();
            }
            let mut buffer = vec![0_u16; length as usize + 1];
            let read = GetWindowTextW(hwnd, &mut buffer);
            String::from_utf16_lossy(&buffer[..read as usize])
        }
    }

    // ------------------------------------------------------------ UI Automation

    struct Uia {
        automation: IUIAutomation,
    }

    impl Uia {
        fn new() -> Result<Self, String> {
            let automation: IUIAutomation = unsafe {
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).map_err(|error| {
                    format!("could not create the UI Automation client: {error}")
                })?
            };
            Ok(Self { automation })
        }

        /// First descendant that can hold editable text. Value pattern is
        /// preferred because it can both write and read; a document with only a
        /// text pattern is read-only to us but still verifies a paste.
        fn editable(&self, hwnd: HWND) -> Result<EditableTarget, String> {
            let root = unsafe {
                self.automation
                    .ElementFromHandle(hwnd)
                    .map_err(|error| format!("no automation element for the window: {error}"))?
            };

            // Collect every text-bearing control rather than picking one.
            // Neither ordering works alone: Word's first Edit is the ribbon
            // search box while its content lives in the Document, and a
            // browser's page Document does not report a textarea's live value
            // while the textarea's own Edit does. Reading across all candidates
            // is what makes one driver work for both.
            let mut elements = Vec::new();
            for control_type in [UIA_DocumentControlTypeId, UIA_EditControlTypeId] {
                let condition = unsafe {
                    self.automation
                        .CreatePropertyCondition(
                            UIA_ControlTypePropertyId,
                            &VARIANT::from(control_type.0),
                        )
                        .map_err(|error| {
                            format!("could not build a control-type condition: {error}")
                        })?
                };
                let found = unsafe { root.FindAll(TreeScope_Descendants, &condition) };
                let Ok(found) = found else { continue };
                let length = unsafe { found.Length() }.unwrap_or(0);
                for index in 0..length.min(MAX_TEXT_CANDIDATES) {
                    if let Ok(element) = unsafe { found.GetElement(index) } {
                        elements.push(element);
                    }
                }
            }

            if elements.is_empty() {
                return Err("no Edit or Document control was found in the window".to_string());
            }
            Ok(EditableTarget { elements })
        }
    }

    /// Enough to cover a document plus the surrounding chrome without walking a
    /// pathological tree.
    const MAX_TEXT_CANDIDATES: i32 = 12;

    struct EditableTarget {
        elements: Vec<IUIAutomationElement>,
    }

    impl EditableTarget {
        /// Focus the candidate whose accessible name contains `marker`.
        ///
        /// A browser window exposes the address bar and the page's own controls
        /// in one tree, and the address bar sorts first. Focusing "the first
        /// editable control" there types into the omnibox -- which still reads
        /// back the payload, so the row would report a pass having never
        /// involved the page at all. The test page labels its textarea so the
        /// row can target it exactly.
        fn has_named(&self, marker: &str) -> bool {
            self.elements.iter().any(|element| {
                unsafe { element.CurrentName() }
                    .map(|name| name.to_string())
                    .unwrap_or_default()
                    .contains(marker)
            })
        }

        fn focus_named(&self, marker: &str) -> Result<(), String> {
            for element in &self.elements {
                let name = unsafe { element.CurrentName() }
                    .map(|name| name.to_string())
                    .unwrap_or_default();
                if name.contains(marker) {
                    return unsafe { element.SetFocus() }
                        .map_err(|error| format!("could not focus '{marker}': {error}"));
                }
            }
            Err(format!("no text control named '{marker}' was found"))
        }

        /// Focus the first candidate, which is the document for a document-based
        /// app.
        fn focus(&self) -> Result<(), String> {
            let mut last_error = None;
            for element in &self.elements {
                match unsafe { element.SetFocus() } {
                    Ok(()) => return Ok(()),
                    Err(error) => last_error = Some(error),
                }
            }
            Err(format!(
                "could not focus any text control: {}",
                last_error
                    .map(|error| error.to_string())
                    .unwrap_or_else(|| "no candidates".to_string())
            ))
        }

        fn clear(&self) {
            for element in &self.elements {
                unsafe {
                    if let Ok(pattern) =
                        element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                    {
                        if pattern
                            .CurrentIsReadOnly()
                            .is_ok_and(|read_only| read_only.as_bool())
                        {
                            continue;
                        }
                        let _ = pattern.SetValue(&windows::core::BSTR::from(""));
                    }
                }
            }
        }

        /// Text visible across every candidate control. The caller looks for the
        /// payload as a substring, so joining is enough and avoids having to
        /// guess which control the paste landed in.
        fn read(&self) -> Result<String, String> {
            let mut parts = Vec::new();
            let mut supported = false;

            for element in &self.elements {
                unsafe {
                    // Word's document exposes a Value pattern that always reads
                    // back empty while the real content is only reachable
                    // through the Text pattern, so an empty Value must fall
                    // through instead of being taken as the contents.
                    if let Ok(pattern) =
                        element.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)
                    {
                        supported = true;
                        if let Ok(value) = pattern.CurrentValue() {
                            let value = value.to_string();
                            if !value.is_empty() {
                                parts.push(value);
                                continue;
                            }
                        }
                    }

                    if let Ok(pattern) =
                        element.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
                    {
                        supported = true;
                        if let Ok(range) = pattern.DocumentRange() {
                            if let Ok(text) = range.GetText(-1) {
                                parts.push(text.to_string());
                            }
                        }
                    }
                }
            }

            if !supported {
                return Err("no control exposes a Value or Text pattern".to_string());
            }
            Ok(parts.join("\n"))
        }
    }

    // ------------------------------------------------------------ app launching

    struct AppCandidate {
        label: &'static str,
        executables: &'static [&'static str],
        args: &'static [&'static str],
        /// When set, a temporary file with this extension is created and passed
        /// as the last argument. Editors that open on a start screen or a
        /// welcome tab have no editable control until a document exists.
        document_ext: Option<&'static str>,
    }

    impl AppCandidate {
        const fn new(
            label: &'static str,
            executables: &'static [&'static str],
            args: &'static [&'static str],
        ) -> Self {
            Self {
                label,
                executables,
                args,
                document_ext: None,
            }
        }

        const fn with_document(
            label: &'static str,
            executables: &'static [&'static str],
            args: &'static [&'static str],
            document_ext: &'static str,
        ) -> Self {
            Self {
                label,
                executables,
                args,
                document_ext: Some(document_ext),
            }
        }
    }

    /// A launched process that is killed when the row finishes, so a failed run
    /// does not leave Word or a browser sitting on the desktop.
    struct LaunchedApp {
        child: Option<Child>,
        keep: bool,
    }

    impl LaunchedApp {
        fn spawn(path: &Path, args: &[&str], keep: bool) -> Result<Self, String> {
            let child = Command::new(path)
                .args(args)
                .spawn()
                .map_err(|error| format!("could not launch {}: {error}", path.display()))?;
            Ok(Self {
                child: Some(child),
                keep,
            })
        }

        fn pid(&self) -> u32 {
            self.child.as_ref().map(Child::id).unwrap_or_default()
        }
    }

    impl Drop for LaunchedApp {
        fn drop(&mut self) {
            if self.keep {
                return;
            }
            if let Some(mut child) = self.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    // ------------------------------------------------------------------- rows

    /// Legacy Win32 and remote-session rows both paste into a plain EDIT control
    /// we own. Owning the target is what makes these two rows dependency-free:
    /// they run on any machine and isolate the paste engine from app quirks.
    fn run_owned_edit_row(class: AppClass, strategy: PasteStrategy) -> RowResult {
        let app = match strategy {
            PasteStrategy::RemoteSession => "owned EDIT control (remote-session strategy)",
            _ => "owned EDIT control",
        };
        let formats = vec![Format::UnicodeText];

        let target = match owned_edit_window() {
            Ok(target) => target,
            Err(error) => {
                return failed(
                    class,
                    app,
                    formats,
                    vec![failure(class, app, Format::UnicodeText, Step::Paste, error)],
                )
            }
        };

        let clipboard = match ClipboardContext::new() {
            Ok(clipboard) => clipboard,
            Err(error) => {
                return failed(
                    class,
                    app,
                    formats,
                    vec![failure(
                        class,
                        app,
                        Format::UnicodeText,
                        Step::Restore,
                        error.to_string(),
                    )],
                )
            }
        };

        let mut failures = Vec::new();
        let mut checks = 0;

        match capture_text_is_storable() {
            Ok(()) => checks += 1,
            Err(error) => failures.push(failure(
                class,
                app,
                Format::UnicodeText,
                Step::Capture,
                error,
            )),
        }
        match restore_text(&clipboard, TEXT_PAYLOAD) {
            Ok(()) => checks += 1,
            Err(error) => failures.push(failure(
                class,
                app,
                Format::UnicodeText,
                Step::Restore,
                error,
            )),
        }

        if failures.is_empty() {
            match owned_edit_paste(&target, strategy) {
                Ok(()) => checks += 1,
                Err(error) => {
                    failures.push(failure(class, app, Format::UnicodeText, Step::Paste, error))
                }
            }
        }

        if failures.is_empty() {
            passed(class, app, formats, checks)
        } else {
            failed(class, app, formats, failures)
        }
    }

    /// Generic row: launch a real application, find its editable control, paste,
    /// and read the text back out of the app itself.
    fn run_uia_app_row(
        class: AppClass,
        candidates: &[AppCandidate],
        options: &Options,
    ) -> RowResult {
        let mut looked_for = Vec::new();
        let mut chosen = None;
        for candidate in candidates {
            for executable in candidate.executables {
                looked_for.push((*executable).to_string());
                if let Some(path) = find_executable(executable) {
                    chosen = Some((candidate, path));
                    break;
                }
            }
            if chosen.is_some() {
                break;
            }
        }

        let Some((candidate, path)) = chosen else {
            return skipped(
                class,
                SkipReason::NoAppInstalled {
                    candidates: looked_for,
                },
            );
        };

        let app = candidate.label;
        let formats = vec![Format::UnicodeText];

        let mut args: Vec<String> = candidate
            .args
            .iter()
            .map(|arg| (*arg).to_string())
            .collect();
        let mut document = None;
        if let Some(extension) = candidate.document_ext {
            let path =
                env::temp_dir().join(format!("cubby-compat-{}.{extension}", unique_suffix()));
            if let Err(error) = std::fs::write(&path, b"") {
                return failed(
                    class,
                    app,
                    formats,
                    vec![failure(
                        class,
                        app,
                        Format::UnicodeText,
                        Step::Paste,
                        format!("could not create a document for {app}: {error}"),
                    )],
                );
            }
            args.push(path.to_string_lossy().into_owned());
            document = Some(path);
        }
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();

        // Located by pid/executable rather than by window title: editors that
        // put the document name in the title bar do exist, but matching on it
        // costs more than it buys -- Word's window is titled before it is ready
        // to accept a paste.
        let outcome = drive_uia_app(class, app, &path, &arg_refs, options.keep_apps, None, None);
        if let Some(document) = document {
            let _ = std::fs::remove_file(document);
        }
        match outcome {
            DriveOutcome::Passed(checks) => passed(class, app, formats, checks),
            DriveOutcome::Failed(failures) => failed(class, app, formats, failures),
            // An app that never exposes an editable element cannot be driven
            // through UI Automation at all. That is a limit of the harness on
            // this machine, not evidence that Cubby's paste is broken, so it is
            // reported as an uncovered class rather than a red run.
            DriveOutcome::NotAutomatable(detail) => skipped(
                class,
                SkipReason::EnvironmentNotApplicable {
                    detail: format!(
                        "{app} exposed no UI Automation text control to paste into: {detail}"
                    ),
                },
            ),
        }
    }

    enum DriveOutcome {
        Passed(usize),
        NotAutomatable(String),
        Failed(Vec<Failure>),
    }

    fn drive_uia_app(
        class: AppClass,
        app: &str,
        path: &Path,
        args: &[&str],
        keep: bool,
        focus_marker: Option<&str>,
        title_marker: Option<&str>,
    ) -> DriveOutcome {
        let mut failures = Vec::new();
        let mut checks = 0;

        let clipboard = match ClipboardContext::new() {
            Ok(clipboard) => clipboard,
            Err(error) => {
                return DriveOutcome::Failed(vec![failure(
                    class,
                    app,
                    Format::UnicodeText,
                    Step::Restore,
                    error.to_string(),
                )])
            }
        };

        match capture_text_is_storable() {
            Ok(()) => checks += 1,
            Err(error) => failures.push(failure(
                class,
                app,
                Format::UnicodeText,
                Step::Capture,
                error,
            )),
        }
        match restore_text(&clipboard, TEXT_PAYLOAD) {
            Ok(()) => checks += 1,
            Err(error) => failures.push(failure(
                class,
                app,
                Format::UnicodeText,
                Step::Restore,
                error,
            )),
        }
        if !failures.is_empty() {
            return DriveOutcome::Failed(failures);
        }

        let launched = match LaunchedApp::spawn(path, args, keep) {
            Ok(launched) => launched,
            Err(error) => {
                return DriveOutcome::Failed(vec![failure(
                    class,
                    app,
                    Format::UnicodeText,
                    Step::Paste,
                    error,
                )])
            }
        };

        let exe_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let uia = match Uia::new() {
            Ok(uia) => uia,
            Err(error) => return DriveOutcome::NotAutomatable(error),
        };

        // Resolve the window and its editable control together, re-reading the
        // window on every attempt. A big application shows a splash or
        // start-screen window first, and latching onto that one means querying
        // an element that never gains a document.
        //
        // Failing to find any editable element is deliberately NOT a row
        // failure: it means the harness cannot drive this app, which is a
        // different claim from "the paste did not work".
        let resolved = wait_for(APP_LAUNCH_TIMEOUT, || {
            // A title marker takes precedence when the caller has one. Browsers
            // are the reason: the launcher process exits once it has handed the
            // URL to the browser, so there is no window under our pid, and
            // falling back to the executable name finds whichever Edge window
            // happened to be open already -- a different profile, without our
            // page in it.
            let window = match title_marker {
                Some(marker) => window_with_title(marker),
                None => {
                    enum_find_window(launched.pid(), "").or_else(|| enum_find_window(0, exe_name))
                }
            };
            let Some(hwnd) = window else {
                return Err(match title_marker {
                    Some(marker) => format!("no visible window titled '{marker}'"),
                    None => format!(
                        "no visible top-level window for pid {} or any {exe_name} process",
                        launched.pid()
                    ),
                });
            };
            match uia.editable(hwnd) {
                // Keep waiting until the specific control exists. A browser
                // exposes its address bar immediately but builds the page's
                // accessibility tree only after the document renders, so
                // accepting the first tree that has any text control at all
                // snapshots the window before the page is in it.
                Ok(target) => match focus_marker {
                    Some(marker) if !target.has_named(marker) => {
                        Err(format!("no text control named '{marker}' yet"))
                    }
                    _ => Ok(Some((hwnd, target))),
                },
                Err(error) => Err(error),
            }
        });
        let (hwnd, target) = match resolved {
            Ok(resolved) => resolved,
            Err(error) => return DriveOutcome::NotAutomatable(error),
        };

        let paste_result = (|| -> Result<(), String> {
            // Activating a window, focusing a control inside it, and having the
            // keystrokes arrive are three separate asynchronous events, and a
            // desktop that has just had several other applications raised in
            // front of it loses these races. Retry the whole sequence rather
            // than reporting a flake as a compatibility failure.
            let mut last = String::new();
            for attempt in 0..PASTE_ATTEMPTS {
                target.clear();

                // Activate first, focus second, paste third. Activating the
                // window can move focus inside it, so focusing a specific
                // control before the window is frontmost is silently undone.
                activate_target(hwnd, PasteStrategy::Standard)?;
                thread::sleep(Duration::from_millis(250));
                match focus_marker {
                    // A named target must be focusable: otherwise the paste
                    // lands elsewhere in the window and the row silently tests
                    // the wrong control -- a browser's address bar, typically,
                    // which reads the payload back and looks like a pass.
                    Some(marker) => target.focus_named(marker)?,
                    // Best effort otherwise: some documents refuse programmatic
                    // focus while still accepting a paste, and the read-back is
                    // the real assertion.
                    None => {
                        let _ = target.focus();
                    }
                }
                send_paste(PasteStrategy::Standard)?;

                let observed = wait_for(PASTE_READBACK_TIMEOUT, || {
                    let text = target.read()?;
                    Ok(text.contains(TEXT_PAYLOAD).then_some(text))
                });
                match observed {
                    Ok(text) => {
                        if text.contains(TEXT_PAYLOAD) {
                            return Ok(());
                        }
                        last = text;
                    }
                    Err(error) => last = error,
                }
                if attempt + 1 < PASTE_ATTEMPTS {
                    thread::sleep(Duration::from_millis(500));
                }
            }

            Err(format!(
                "pasted text never appeared in the target control after {PASTE_ATTEMPTS} attempts (last read: {:?})",
                truncate(&last)
            ))
        })();

        match paste_result {
            Ok(()) => checks += 1,
            Err(error) => {
                failures.push(failure(class, app, Format::UnicodeText, Step::Paste, error))
            }
        }

        // Killing the child is not enough for apps whose launcher hands off to
        // another process and exits -- a browser being the usual case -- so the
        // window itself is asked to close. Without this every run leaves one
        // more window on the desktop.
        if !keep {
            close_window(hwnd);
        }

        if failures.is_empty() {
            DriveOutcome::Passed(checks)
        } else {
            DriveOutcome::Failed(failures)
        }
    }

    fn close_window(hwnd: HWND) {
        use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_CLOSE, Default::default(), Default::default());
        }
    }

    fn truncate(text: &str) -> String {
        let trimmed: String = text.chars().take(80).collect();
        if text.chars().count() > 80 {
            format!("{trimmed}...")
        } else {
            trimmed
        }
    }

    /// Browsers refuse top-level `data:` navigation, so the row writes a real
    /// HTML file with a textarea and opens it over `file://`.
    fn run_browser_row(class: AppClass, options: &Options) -> RowResult {
        let candidates = ["msedge.exe", "chrome.exe", "firefox.exe"];
        let mut looked_for = Vec::new();
        let mut chosen = None;
        for executable in candidates {
            looked_for.push(executable.to_string());
            if let Some(path) = find_executable(executable) {
                chosen = Some((executable, path));
                break;
            }
        }
        let Some((app, path)) = chosen else {
            return skipped(
                class,
                SkipReason::NoAppInstalled {
                    candidates: looked_for,
                },
            );
        };

        let page_title = format!("CubbyCompatPage{}", unique_suffix());
        let page = match write_browser_page(&page_title) {
            Ok(page) => page,
            Err(error) => {
                return failed(
                    class,
                    app,
                    vec![Format::UnicodeText],
                    vec![failure(class, app, Format::UnicodeText, Step::Paste, error)],
                )
            }
        };
        let url = format!("file:///{}", page.to_string_lossy().replace('\\', "/"));
        // A private profile directory is what makes the other flags mean
        // anything: an already-running browser would otherwise adopt the URL
        // into its existing process and silently drop the command line, leaving
        // the row driving somebody else's window.
        let profile = env::temp_dir().join(format!("cubby-compat-profile-{}", unique_suffix()));
        let profile_arg = format!("--user-data-dir={}", profile.to_string_lossy());
        // Chromium only builds an accessibility tree for page content when an
        // assistive client asks for it. Without this flag the textarea is
        // invisible to UI Automation and the only Edit control in the window is
        // the address bar -- which a naive driver will happily paste into and
        // call a pass.
        let args: Vec<&str> = vec![
            &profile_arg,
            "--no-first-run",
            "--no-default-browser-check",
            "--new-window",
            "--force-renderer-accessibility",
            &url,
        ];

        let formats = vec![Format::UnicodeText];
        let outcome = drive_uia_app(
            class,
            app,
            &path,
            &args,
            options.keep_apps,
            Some(BROWSER_TARGET_LABEL),
            Some(&page_title),
        );
        let _ = std::fs::remove_file(&page);
        if !options.keep_apps {
            let _ = std::fs::remove_dir_all(&profile);
        }
        match outcome {
            DriveOutcome::Passed(checks) => passed(class, app, formats, checks),
            DriveOutcome::Failed(failures) => failed(class, app, formats, failures),
            DriveOutcome::NotAutomatable(detail) => skipped(
                class,
                SkipReason::EnvironmentNotApplicable {
                    detail: format!(
                        "{app} exposed no UI Automation text control to paste into: {detail}"
                    ),
                },
            ),
        }
    }

    /// Accessible name of the test page's textarea, used to focus exactly that
    /// control rather than the address bar.
    const BROWSER_TARGET_LABEL: &str = "cubby paste target";

    /// The page carries a unique title so the row can find its own browser
    /// window, and a labelled textarea so it can find the right control inside
    /// that window.
    fn write_browser_page(title: &str) -> Result<PathBuf, String> {
        let path = env::temp_dir().join(format!("cubby-compat-{}.html", unique_suffix()));
        let html = format!(
            "<!doctype html><meta charset=\"utf-8\"><title>{title}</title>\
             <textarea id=\"t\" autofocus rows=\"8\" cols=\"60\" \
             aria-label=\"{BROWSER_TARGET_LABEL}\"></textarea>"
        );
        std::fs::write(&path, html).map_err(|error| error.to_string())?;
        Ok(path)
    }

    /// Console paste goes through conhost rather than the window's edit control,
    /// so the row lets `cmd.exe` read one line and write it to a file. The file
    /// is the assertion: it proves the console actually received the text.
    fn run_terminal_row(class: AppClass) -> RowResult {
        let app = "powershell.exe in Windows Terminal";
        let Some(terminal_path) = find_executable("wt.exe") else {
            return skipped(
                class,
                SkipReason::NoAppInstalled {
                    candidates: vec!["wt.exe".to_string()],
                },
            );
        };
        let formats = vec![Format::UnicodeText];
        match drive_terminal(class, app, &terminal_path) {
            Ok(checks) => passed(class, app, formats, checks),
            Err(failures) => failed(class, app, formats, failures),
        }
    }

    fn drive_terminal(
        class: AppClass,
        app: &str,
        terminal_path: &Path,
    ) -> Result<usize, Vec<Failure>> {
        let mut checks = 0;
        let clipboard = ClipboardContext::new().map_err(|error| {
            vec![failure(
                class,
                app,
                Format::UnicodeText,
                Step::Restore,
                error.to_string(),
            )]
        })?;

        capture_text_is_storable().map_err(|error| {
            vec![failure(
                class,
                app,
                Format::UnicodeText,
                Step::Capture,
                error,
            )]
        })?;
        checks += 1;
        restore_text(&clipboard, TEXT_PAYLOAD).map_err(|error| {
            vec![failure(
                class,
                app,
                Format::UnicodeText,
                Step::Restore,
                error,
            )]
        })?;
        checks += 1;

        let suffix = unique_suffix();
        let file_name = format!("cubby-compat-console-{suffix}.txt");
        let output = env::temp_dir().join(&file_name);
        let _ = std::fs::remove_file(&output);
        // Three Windows details are load-bearing here:
        //  * the console is launched with `wt -w new`. Spawning cmd.exe with
        //    CREATE_NEW_CONSOLE opens a new *tab* in the existing Windows
        //    Terminal window on Windows 11, leaving no top-level window of our
        //    own to paste into, and routing through conhost.exe instead makes
        //    conhost take over this process's console. `-w new` forces a fresh
        //    window and `--title` makes it findable, since the window belongs to
        //    WindowsTerminal.exe rather than to the pid we spawned;
        //  * `/v:on` plus `!line!` is required, because `%line%` would be
        //    expanded when the command line is parsed -- before `set /p` reads;
        //  * the command must contain no double quotes. Rust escapes inner
        //    quotes as \" when it builds the command line, which cmd.exe does
        //    not understand, so `cd /d` moves to the temp directory (taking the
        //    rest of the line, spaces and all) and the redirect target is then a
        //    bare file name that never needs quoting.
        let title = format!("CubbyCompatConsole{suffix}");
        // PowerShell rather than cmd.exe. `cmd`'s `set /p` reads console input
        // through the console code page and returns "caf?" for "café" even
        // after `chcp 65001`, which would report a Cubby paste bug that is
        // really a cmd.exe limitation. `Read-Host` is UTF-16 throughout, so a
        // mismatch here is a genuine paste failure.
        //
        // `$env:TEMP\<name>` keeps the command free of double quotes: Rust
        // escapes inner quotes as \" when building the command line, which
        // neither cmd nor PowerShell parses the way it is intended.
        let command =
            format!("Read-Host | Set-Content -LiteralPath $env:TEMP\\{file_name} -Encoding utf8");

        let result = (|| -> Result<(), String> {
            let mut launched = LaunchedApp::spawn(
                terminal_path,
                &[
                    "-w",
                    "new",
                    "--title",
                    &title,
                    "powershell.exe",
                    "-NoProfile",
                    "-Command",
                    &command,
                ],
                false,
            )?;
            let hwnd = wait_for(DEFAULT_TIMEOUT, || Ok(window_with_title(&title)))
                .map_err(|_| "the cmd.exe console window never appeared".to_string())?;
            paste_into(hwnd, PasteStrategy::Standard)?;
            thread::sleep(Duration::from_millis(300));
            send_enter()?;

            let observed = wait_for(DEFAULT_TIMEOUT, || match std::fs::read(&output) {
                Ok(bytes) if !bytes.is_empty() => Ok(Some(decode_console_bytes(&bytes))),
                _ => Ok(None),
            })
            .map_err(|_| "the shell never wrote the pasted line".to_string())?;

            if let Some(child) = launched.child.as_mut() {
                let _ = child.wait();
            }

            let observed = observed.trim_end_matches(['\r', '\n']).to_string();
            if observed != TEXT_PAYLOAD {
                return Err(format!(
                    "console received {:?} instead of the payload",
                    truncate(&observed)
                ));
            }
            Ok(())
        })();

        let _ = std::fs::remove_file(&output);
        match result {
            Ok(()) => Ok(checks + 1),
            Err(error) => Err(vec![failure(
                class,
                app,
                Format::UnicodeText,
                Step::Paste,
                error,
            )]),
        }
    }

    /// `echo` writes the console's OEM code page. Try UTF-8 first (which modern
    /// Windows consoles use) and fall back to a lossy read so a decode problem
    /// surfaces as a text mismatch rather than an unrelated error.
    fn decode_console_bytes(bytes: &[u8]) -> String {
        let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
        match std::str::from_utf8(bytes) {
            Ok(text) => text.to_string(),
            Err(_) => String::from_utf8_lossy(bytes).into_owned(),
        }
    }

    fn send_enter() -> Result<(), String> {
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_RETURN,
        };
        let inputs = [
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VK_RETURN,
                        ..Default::default()
                    },
                },
            },
            INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: VK_RETURN,
                        dwFlags: KEYEVENTF_KEYUP,
                        ..Default::default()
                    },
                },
            },
        ];
        let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
        if sent as usize != inputs.len() {
            return Err(format!(
                "SendInput accepted {sent} of {} events",
                inputs.len()
            ));
        }
        Ok(())
    }

    /// Explorer is the file-payload row. Cubby deliberately does not store file
    /// payloads, so the assertion is the product contract: the payload is
    /// classified as ignored, Explorer still pastes it, and a later text copy is
    /// still captured -- an ignored payload must not wedge capture.
    fn run_explorer_row(class: AppClass) -> RowResult {
        let app = "explorer.exe";
        let formats = vec![Format::FilePayload, Format::UnicodeText];
        match drive_explorer(class, app) {
            Ok(checks) => passed(class, app, formats, checks),
            Err(failures) => failed(class, app, formats, failures),
        }
    }

    fn drive_explorer(class: AppClass, app: &str) -> Result<usize, Vec<Failure>> {
        let mut checks = 0;
        let suffix = unique_suffix();
        let source_dir = env::temp_dir().join(format!("cubby-compat-src-{suffix}"));
        let dest_dir = env::temp_dir().join(format!("cubby-compat-dst-{suffix}"));
        let file_name = "cubby-compat-payload.txt";
        let source_file = source_dir.join(file_name);

        let setup = (|| -> Result<(), String> {
            std::fs::create_dir_all(&source_dir).map_err(|error| error.to_string())?;
            std::fs::create_dir_all(&dest_dir).map_err(|error| error.to_string())?;
            std::fs::write(&source_file, TEXT_PAYLOAD).map_err(|error| error.to_string())
        })();
        if let Err(error) = setup {
            return Err(vec![failure(
                class,
                app,
                Format::FilePayload,
                Step::Restore,
                error,
            )]);
        }

        // capture: a file payload with no image alongside it is not durable
        // history, so Cubby must classify it as ignored.
        let policy = classify_file_payload(true, false);
        if policy != FilePayloadPolicy::IgnoreFilePayload {
            return Err(vec![failure(
                class,
                app,
                Format::FilePayload,
                Step::Capture,
                format!("file payload classified as {policy:?}, expected IgnoreFilePayload"),
            )]);
        }
        checks += 1;

        let clipboard = ClipboardContext::new().map_err(|error| {
            vec![failure(
                class,
                app,
                Format::FilePayload,
                Step::Restore,
                error.to_string(),
            )]
        })?;

        let mut failures = Vec::new();
        let paste_result = (|| -> Result<(), String> {
            clipboard
                .set(vec![ClipboardContent::Files(vec![source_file
                    .to_string_lossy()
                    .into_owned()])])
                .map_err(|error| {
                    format!("could not put the file payload on the clipboard: {error}")
                })?;
            thread::sleep(Duration::from_millis(200));

            let mut launched = LaunchedApp::spawn(
                Path::new("explorer.exe"),
                &[&dest_dir.to_string_lossy()],
                false,
            )?;
            // explorer.exe hands off to the running shell process and exits, so
            // the window belongs to a different pid than the one we spawned.
            let _ = launched.child.as_mut().map(|child| child.wait());
            let hwnd = wait_for(DEFAULT_TIMEOUT, || Ok(explorer_window_for(&dest_dir)))
                .map_err(|_| format!("no Explorer window opened on {}", dest_dir.display()))?;

            paste_into(hwnd, PasteStrategy::Standard)?;

            wait_for(DEFAULT_TIMEOUT, || {
                Ok(dest_dir.join(file_name).is_file().then_some(()))
            })
            .map_err(|_| "Explorer did not paste the file into the destination".to_string())?;
            Ok(())
        })();

        match paste_result {
            Ok(()) => checks += 1,
            Err(error) => {
                failures.push(failure(class, app, Format::FilePayload, Step::Paste, error))
            }
        }

        // The contract's second half: an ignored payload must not block the next
        // real capture.
        match restore_text(&clipboard, TEXT_PAYLOAD) {
            Ok(()) => checks += 1,
            Err(error) => failures.push(failure(
                class,
                app,
                Format::UnicodeText,
                Step::Capture,
                format!("clipboard was left unusable after an ignored file payload: {error}"),
            )),
        }

        let _ = std::fs::remove_dir_all(&source_dir);
        let _ = std::fs::remove_dir_all(&dest_dir);

        if failures.is_empty() {
            Ok(checks)
        } else {
            Err(failures)
        }
    }

    /// Explorer windows are owned by the long-running shell process, so they are
    /// found by title rather than by the pid we spawned.
    fn explorer_window_for(directory: &Path) -> Option<HWND> {
        let leaf = directory.file_name()?.to_string_lossy().into_owned();
        window_with_title(&leaf)
    }

    /// Find a visible top-level window whose title contains `needle`. Used for
    /// windows that are not owned by the process we launched: Explorer windows
    /// belong to the running shell, and console windows belong to conhost.
    fn window_with_title(needle: &str) -> Option<HWND> {
        let mut search = TitleSearch {
            needle: needle.to_string(),
            found: HWND::default(),
        };
        unsafe {
            let _ = EnumWindows(
                Some(enum_title_proc),
                LPARAM(&mut search as *mut TitleSearch as isize),
            );
        }
        (!search.found.0.is_null()).then_some(search.found)
    }

    struct TitleSearch {
        needle: String,
        found: HWND,
    }

    unsafe extern "system" fn enum_title_proc(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
        let search = &mut *(lparam.0 as *mut TitleSearch);
        if IsWindowVisible(hwnd).as_bool() && window_text(hwnd).contains(&search.needle) {
            search.found = hwnd;
            return windows::core::BOOL(0);
        }
        TRUE
    }

    /// UIPI blocks synthetic input from a medium-integrity process into an
    /// elevated window. The row asserts whichever half of that contract this
    /// machine can actually demonstrate, and skips when it can demonstrate
    /// neither.
    fn run_elevated_row(class: AppClass) -> RowResult {
        if !process_is_elevated() {
            return skipped(
                class,
                SkipReason::EnvironmentNotApplicable {
                    detail: "this run is not elevated; start an elevated shell and re-run to \
                             exercise elevated targets (UIPI blocks synthetic paste into an \
                             elevated window from a medium-integrity process by design)"
                        .to_string(),
                },
            );
        }

        // Running elevated, an elevated target is reachable: exercise the same
        // owned-control paste, which now happens at high integrity.
        let mut result = run_owned_edit_row(class, PasteStrategy::Standard);
        result.app = "owned EDIT control (elevated)".to_string();
        result
    }

    fn process_is_elevated() -> bool {
        use windows::Win32::Security::{
            GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
        };
        use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

        unsafe {
            let mut token = HANDLE::default();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
                return false;
            }
            let mut elevation = TOKEN_ELEVATION::default();
            let mut size = 0_u32;
            let ok = GetTokenInformation(
                token,
                TokenElevation,
                Some(&mut elevation as *mut TOKEN_ELEVATION as *mut std::ffi::c_void),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut size,
            )
            .is_ok();
            let _ = CloseHandle(token);
            ok && elevation.TokenIsElevated != 0
        }
    }

    // ------------------------------------------------- owned EDIT control target

    struct OwnedEdit {
        window: HWND,
        edit: HWND,
    }

    fn owned_edit_window() -> Result<OwnedEdit, String> {
        use windows::core::w;
        use windows::Win32::Foundation::HINSTANCE;
        use windows::Win32::Graphics::Gdi::UpdateWindow;
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, RegisterClassW, ShowWindow, CS_HREDRAW, CS_VREDRAW,
            CW_USEDEFAULT, ES_AUTOHSCROLL, ES_AUTOVSCROLL, ES_MULTILINE, SW_SHOW, WINDOW_EX_STYLE,
            WINDOW_STYLE, WNDCLASSW, WS_CHILD, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
        };

        unsafe extern "system" fn window_proc(
            hwnd: HWND,
            message: u32,
            wparam: windows::Win32::Foundation::WPARAM,
            lparam: LPARAM,
        ) -> windows::Win32::Foundation::LRESULT {
            DefWindowProcW(hwnd, message, wparam, lparam)
        }

        unsafe {
            let module = GetModuleHandleW(None).map_err(|error| error.to_string())?;
            let instance = HINSTANCE(module.0);
            let class_name = w!("CubbyCompatMatrixTarget");
            RegisterClassW(&WNDCLASSW {
                style: CS_HREDRAW | CS_VREDRAW,
                hInstance: instance,
                lpszClassName: class_name,
                lpfnWndProc: Some(window_proc),
                ..Default::default()
            });

            let window = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class_name,
                w!("Cubby Compatibility Target"),
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
            .map_err(|error| error.to_string())?;

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
                Some(window),
                None,
                Some(instance),
                None,
            )
            .map_err(|error| error.to_string())?;

            let _ = ShowWindow(window, SW_SHOW);
            let _ = UpdateWindow(window);
            pump_messages();
            Ok(OwnedEdit { window, edit })
        }
    }

    fn owned_edit_paste(target: &OwnedEdit, strategy: PasteStrategy) -> Result<(), String> {
        use windows::core::w;
        use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
        use windows::Win32::UI::WindowsAndMessaging::{
            DestroyWindow, SetForegroundWindow, SetWindowTextW,
        };

        unsafe {
            SetWindowTextW(target.edit, w!("")).map_err(|error| error.to_string())?;
            let _ = SetForegroundWindow(target.window);
            SetFocus(Some(target.edit)).map_err(|error| error.to_string())?;
            pump_messages();

            paste_into(target.window, strategy)?;
            SetFocus(Some(target.edit)).map_err(|error| error.to_string())?;

            let observed = wait_for(DEFAULT_TIMEOUT, || {
                pump_messages();
                let text = window_text(target.edit);
                Ok((!text.is_empty()).then_some(text))
            })
            .map_err(|_| "the paste never reached the owned EDIT control".to_string())?;

            let _ = DestroyWindow(target.window);

            if observed != TEXT_PAYLOAD {
                return Err(format!(
                    "control received {:?} instead of the payload",
                    truncate(&observed)
                ));
            }
            Ok(())
        }
    }

    fn pump_messages() {
        use windows::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
        };
        unsafe {
            let mut message = MSG::default();
            while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
}
