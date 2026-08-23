//! Native clipboard-write IPC policy (SBS-1042).
//!
//! Paste, copy, recognized-text restore, and image-selection copy write the
//! OS clipboard from the native process. They used to also
//! `emit("clipboard-write", decrypted_body)`. The frontend has no listener
//! for that event, so the payload sat in WebView memory for a dead event.
//!
//! Restore/copy must not emit it, and must not decrypt solely to produce a
//! payload for it. Isolated so `cargo test` can pin the call sites without a
//! Tauri window. `rustc --test src-tauri/src/clipboard_write_ipc.rs` runs
//! these tests on Linux.

/// Event that used to carry the full decrypted clip after paste/copy.
/// The frontend has no listener. Do not emit it.
const DEAD_CLIPBOARD_WRITE_EVENT: &str = "clipboard-write";

#[cfg(test)]
mod tests {
    use super::DEAD_CLIPBOARD_WRITE_EVENT;

    fn rust_fn_body<'a>(src: &'a str, needle: &str) -> &'a str {
        let start = src
            .find(needle)
            .unwrap_or_else(|| panic!("{needle} should exist"));
        let rest = &src[start..];
        let brace = rest.find('{').expect("function should have a body");
        let body_start = start + brace;
        let bytes = src.as_bytes();
        let mut depth = 0usize;
        for (offset, &byte) in bytes[body_start..].iter().enumerate() {
            match byte {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        return &src[body_start..=body_start + offset];
                    }
                }
                _ => {}
            }
        }
        panic!("{needle} body was unbalanced");
    }

    /// `.emit("clipboard-write"` including a line-broken argument list.
    fn emits_clipboard_write(src: &str) -> bool {
        let mut rest = src;
        while let Some(idx) = rest.find(".emit(") {
            let after = rest[idx + ".emit(".len()..].trim_start();
            if after.starts_with(&format!("\"{DEAD_CLIPBOARD_WRITE_EVENT}\"")) {
                return true;
            }
            rest = &rest[idx + ".emit(".len()..];
        }
        false
    }

    /// The three sites named in SBS-1042. A file-wide scan still runs below
    /// so a fourth copy of the same dead event cannot hide next to them.
    #[test]
    fn restore_and_copy_sites_do_not_emit_clipboard_write() {
        let src = include_str!("commands.rs");
        let mut leaked = Vec::new();
        for needle in [
            "async fn restore_clip(",
            "async fn restore_recognized_text(",
            "pub async fn copy_selected_text(",
        ] {
            let body = rust_fn_body(src, needle);
            if emits_clipboard_write(body) {
                leaked.push(needle);
            }
        }
        assert!(
            leaked.is_empty(),
            "fail-without-fix: {} emit clipboard-write with a decrypted body (SBS-1042)",
            leaked.join(", ")
        );

        // restore_clip used to build `String::from_utf8_lossy(&clip.content)`
        // solely for that emit. The clipboard write itself goes through
        // `clipboard_contents_for_restore`; this body must not materialize
        // the plaintext a second time for the WebView.
        let restore_clip = rust_fn_body(src, "async fn restore_clip(");
        assert!(
            !restore_clip.contains("from_utf8_lossy(&clip.content)"),
            "fail-without-fix: restore_clip still materializes the decrypted body for the WebView"
        );
    }

    #[test]
    fn native_sources_do_not_emit_clipboard_write() {
        // Sweep every native emit site, not just the three named in the ticket.
        // clipboard-change is the live event and is allowed; clipboard-write is not.
        let sources = [
            ("commands.rs", include_str!("commands.rs")),
            ("clipboard.rs", include_str!("clipboard.rs")),
            ("lib.rs", include_str!("lib.rs")),
            ("settings_commands.rs", include_str!("settings_commands.rs")),
            ("ocr_queue.rs", include_str!("ocr_queue.rs")),
        ];
        for (name, src) in sources {
            assert!(
                !emits_clipboard_write(src),
                "fail-without-fix: {name} emits clipboard-write (SBS-1042)"
            );
        }
    }

    #[test]
    fn frontend_has_no_clipboard_write_listener() {
        // The event is dead. A new listener that expected the decrypted body
        // would be a reason to reintroduce the emit; fail here instead.
        let sources = [
            ("App.tsx", include_str!("../../frontend/src/App.tsx")),
            (
                "HistoryWindow.tsx",
                include_str!("../../frontend/src/windows/HistoryWindow.tsx"),
            ),
            (
                "ImageWindow.tsx",
                include_str!("../../frontend/src/windows/ImageWindow.tsx"),
            ),
            (
                "SettingsPanel.tsx",
                include_str!("../../frontend/src/components/SettingsPanel.tsx"),
            ),
        ];
        for (name, src) in sources {
            assert!(
                !src.contains(DEAD_CLIPBOARD_WRITE_EVENT),
                "fail-without-fix: {name} mentions clipboard-write; restore/copy do not emit it (SBS-1042)"
            );
        }
    }
}
