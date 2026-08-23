//! Focus gate after the paste settle delay (SBS-1066).
//!
//! Restore already refuses to synthesize Ctrl+V when the remembered target
//! does not have focus. The settle sleep used to sit *after* that check, so a
//! click-away during 100-600 ms still received Ctrl+V. This helper is the
//! post-sleep gate: send only if the intended target still has focus.
//!
//! Isolated so `rustc --test src-tauri/src/paste_after_settle.rs` runs on
//! Linux. A live `GetForegroundWindow` pin after a real desktop sleep needs an
//! interactive Windows session (`paste_compat` / `compat_matrix`). CI does not
//! run those harnesses; the unit tests here inject settle and focus.

/// After `settle` returns, send only if the intended target still has focus.
///
/// `None` means focus moved during the delay. The clip is already on the
/// clipboard; the caller leaves it for a manual paste instead of guessing.
pub fn send_paste_after_settle<T>(
    settle: impl FnOnce(),
    target_has_focus: impl FnOnce() -> bool,
    send: impl FnOnce() -> T,
) -> Option<T> {
    settle();
    if !target_has_focus() {
        return None;
    }
    Some(send())
}

#[cfg(test)]
mod tests {
    use super::send_paste_after_settle;

    #[test]
    fn lost_focus_during_settle_does_not_send() {
        let mut sent = false;
        let result = send_paste_after_settle(
            || {},
            || false,
            || {
                sent = true;
                4_u32
            },
        );
        assert_eq!(
            result, None,
            "fail-without-fix: Ctrl+V was synthesized after focus moved (SBS-1066)"
        );
        assert!(
            !sent,
            "fail-without-fix: send ran after the target lost focus (SBS-1066)"
        );
    }

    #[test]
    fn still_focused_after_settle_sends() {
        let mut sent = false;
        let result = send_paste_after_settle(
            || {},
            || true,
            || {
                sent = true;
                4_u32
            },
        );
        assert_eq!(result, Some(4));
        assert!(sent);
    }

    #[test]
    fn settle_runs_before_the_focus_recheck() {
        let order = std::cell::RefCell::new(Vec::new());
        send_paste_after_settle(
            || order.borrow_mut().push("settle"),
            || {
                order.borrow_mut().push("focus");
                true
            },
            || {
                order.borrow_mut().push("send");
                4_u32
            },
        );
        assert_eq!(*order.borrow(), ["settle", "focus", "send"]);
    }

    #[test]
    fn lost_focus_still_settles_first() {
        let order = std::cell::RefCell::new(Vec::new());
        let result = send_paste_after_settle(
            || order.borrow_mut().push("settle"),
            || {
                order.borrow_mut().push("focus");
                false
            },
            || {
                order.borrow_mut().push("send");
                4_u32
            },
        );
        assert_eq!(result, None);
        assert_eq!(
            *order.borrow(),
            ["settle", "focus"],
            "fail-without-fix: send ran after settle without a focus re-check (SBS-1066)"
        );
    }

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

    /// The hide callbacks used to sleep then call `send_paste_input` with no
    /// intervening focus check. Both sites must go through the shared helper.
    #[test]
    fn restore_sites_use_the_shared_focus_gated_helper() {
        let src = include_str!("commands.rs");
        for needle in [
            "async fn restore_clip(",
            "async fn restore_recognized_text(",
        ] {
            let body = rust_fn_body(src, needle);
            assert!(
                body.contains("complete_auto_paste_after_hide"),
                "fail-without-fix: {needle} does not use the shared helper (SBS-1066)"
            );
            assert!(
                !body.contains("paste_settle_delay"),
                "fail-without-fix: {needle} still sleeps the settle delay inline (SBS-1066)"
            );
            assert!(
                !body.contains("send_paste_input"),
                "fail-without-fix: {needle} still sends Ctrl+V inline after settle (SBS-1066)"
            );
        }
    }

    #[test]
    fn production_helper_rechecks_focus_after_settle() {
        let src = include_str!("paste_engine.rs");
        let hide = rust_fn_body(src, "pub fn complete_auto_paste_after_hide(");
        assert!(
            hide.contains("restore_previous_foreground_window"),
            "the shared helper must keep the pre-sleep restore gate"
        );
        assert!(
            hide.contains("send_settled_paste_input"),
            "fail-without-fix: complete_auto_paste_after_hide does not use the settle gate (SBS-1066)"
        );

        let settled = rust_fn_body(src, "pub fn send_settled_paste_input(");
        assert!(
            settled.contains("send_paste_after_settle"),
            "fail-without-fix: send_settled_paste_input skips the post-sleep focus gate (SBS-1066)"
        );
        assert!(
            settled.contains("previous_target_has_focus"),
            "fail-without-fix: send_settled_paste_input does not re-check the remembered target (SBS-1066)"
        );
    }
}
