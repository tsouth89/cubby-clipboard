/// Destinations Cubby installs for `tauri-plugin-log`.
///
/// Kept as a plain enum so the debug/release lists can be unit-tested without
/// constructing a Tauri plugin builder, and without the Windows-only crate
/// graph. `rustc --test src-tauri/src/log_targets.rs` runs these tests on Linux.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LogTarget {
    Stdout,
    Webview,
    LogDir,
}

/// Where a release `LogDir` target actually writes.
///
/// `tauri-plugin-log`'s `TargetKind::LogDir` always resolves to the Tauri
/// AppData log path (`%LOCALAPPDATA%\<identifier>\logs` on Windows), even
/// when the rest of the app is running from a portable folder. Portable mode
/// promises no AppData footprint (SBS-776), so a known portable root maps to
/// a folder inside that root instead.
///
/// `None` is the installed run. This function does not probe the executable;
/// the caller passes the already-resolved portable data root (or `None`) so
/// both arms are testable without a `portable.txt` beside the test binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PersistentLogSink {
    Folder(std::path::PathBuf),
    OsLogDir,
}

/// Targets for a debug (`true`) or release (`false`) build.
///
/// Debug keeps Stdout + Webview so `tauri dev` can show Rust logs in the
/// console. Release stays on disk (`LogDir`) only: streaming into the WebView
/// would put process names, clip UUIDs, and filter values in renderer memory
/// (SBS-837).
pub(crate) fn log_targets(debug_assertions: bool) -> &'static [LogTarget] {
    if debug_assertions {
        &[LogTarget::Stdout, LogTarget::Webview]
    } else {
        &[LogTarget::LogDir]
    }
}

/// Choose the persistent-log sink for a known portable root, or installed.
///
/// SBS-776: a portable root must not select the OS / Tauri AppData log
/// directory. An installed run (`None`) must keep using it.
pub(crate) fn persistent_log_sink(portable_root: Option<std::path::PathBuf>) -> PersistentLogSink {
    match portable_root {
        Some(root) => PersistentLogSink::Folder(root.join("logs")),
        None => PersistentLogSink::OsLogDir,
    }
}

/// Where a portable build keeps its logs, given its data root.
///
/// `Some` only when the run is portable, so storage measurement can skip
/// exactly this folder and only then. Installed runs return `None` and keep
/// their logs under `%LOCALAPPDATA%`, outside the history data directory.
pub(crate) fn portable_log_dir(
    portable_root: Option<std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
    match persistent_log_sink(portable_root) {
        PersistentLogSink::Folder(path) => Some(path),
        PersistentLogSink::OsLogDir => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{log_targets, persistent_log_sink, portable_log_dir, LogTarget, PersistentLogSink};
    use std::path::{Path, PathBuf};

    #[test]
    fn production_targets_do_not_include_webview() {
        assert!(
            !log_targets(false).contains(&LogTarget::Webview),
            "release builds must not stream Rust logs into the WebView"
        );
    }

    #[test]
    fn production_targets_still_include_log_dir() {
        assert!(
            log_targets(false).contains(&LogTarget::LogDir),
            "release builds must keep writing logs to disk"
        );
    }

    #[test]
    fn debug_targets_include_stdout_and_webview() {
        let targets = log_targets(true);
        assert!(
            targets.contains(&LogTarget::Stdout),
            "debug builds must keep Stdout"
        );
        assert!(
            targets.contains(&LogTarget::Webview),
            "debug builds may keep Webview"
        );
    }

    /// Portable logs belong inside the portable data root, not AppData.
    /// Selecting `OsLogDir` here is the SBS-776 failure: `TargetKind::LogDir`
    /// then creates `%LOCALAPPDATA%\<identifier>\logs`.
    #[test]
    fn portable_root_selects_a_folder_inside_that_root() {
        let root = PathBuf::from(r"D:\USB\Cubby\data");
        match persistent_log_sink(Some(root.clone())) {
            PersistentLogSink::Folder(path) => {
                assert_eq!(path, root.join("logs"));
                assert!(
                    path.starts_with(&root),
                    "portable logs must stay under the portable data root"
                );
            }
            PersistentLogSink::OsLogDir => {
                panic!("a portable root must not select the Tauri AppData log directory")
            }
        }
    }

    /// An installed build must keep using the OS log directory. `None` is what
    /// tells the `LogTarget::LogDir` mapping to fall back to
    /// `TargetKind::LogDir`.
    #[test]
    fn installed_run_keeps_the_os_log_directory() {
        assert_eq!(persistent_log_sink(None), PersistentLogSink::OsLogDir);
        assert_eq!(portable_log_dir(None), None);
    }

    /// Clean-profile smoke at the path-selection layer: a brand-new portable
    /// data root must resolve under itself and must not look like Tauri
    /// AppData (`%LOCALAPPDATA%\<identifier>\logs`).
    #[test]
    fn clean_portable_profile_does_not_select_appdata() {
        let root = std::env::temp_dir()
            .join("cubby-sbs-776-clean-profile")
            .join("data");
        let sink = persistent_log_sink(Some(root.clone()));
        let PersistentLogSink::Folder(path) = sink else {
            panic!("clean portable profile selected OsLogDir");
        };
        assert_eq!(path, root.join("logs"));
        assert!(path.starts_with(&root));
        assert!(
            !looks_like_tauri_appdata_log_path(&path),
            "portable sink looked like Tauri AppData: {}",
            path.display()
        );
    }

    fn looks_like_tauri_appdata_log_path(path: &Path) -> bool {
        let rendered = path.to_string_lossy();
        rendered.contains("AppData")
            || rendered.contains("LOCALAPPDATA")
            || rendered.contains("ai.southforge.cubbyclipboard")
    }
}
