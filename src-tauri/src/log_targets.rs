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

#[cfg(test)]
mod tests {
    use super::{log_targets, LogTarget};

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
}
