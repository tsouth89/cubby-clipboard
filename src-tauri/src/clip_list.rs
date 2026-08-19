//! List-row IPC mapping.
//!
//! Isolated so the `preview_only` contract can be unit-tested without compiling
//! the Windows-only crate. `commands.rs` is the only caller. `get_clips` and
//! `search_clips` both go through these helpers (SBS-829 / SBS-912).

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};

/// What the list IPC puts in `content`.
///
/// Hidden rows ship nothing. Image rows keep the thumbnail (`imageSrcFromContent`
/// reads this field). Text rows ship the full decrypted body only when
/// `preview_only` is false; otherwise `content` is empty and the row uses
/// `preview`.
pub fn list_item_content(
    clip_type: &str,
    decrypted: &[u8],
    preview_only: bool,
    is_hidden: bool,
) -> String {
    if is_hidden {
        return String::new();
    }
    if clip_type == "image" {
        return BASE64.encode(decrypted);
    }
    if preview_only {
        return String::new();
    }
    String::from_utf8_lossy(decrypted).to_string()
}

/// What the list IPC puts in `preview`. Hidden rows stay fully blanked.
pub fn list_item_preview(preview: &str, is_hidden: bool) -> String {
    if is_hidden {
        String::new()
    } else {
        preview.to_string()
    }
}

/// Notes on a hidden row can name the secret, so they are withheld too.
pub fn list_item_notes(notes: Option<&str>, is_hidden: bool) -> Option<String> {
    if is_hidden {
        None
    } else {
        notes.map(str::to_string)
    }
}

/// Search IPC `preview_only`. Always true.
///
/// Flyout and History are the only callers and both are list UIs, so no search
/// result ever needs a body. The argument stays so an existing caller that
/// still sends `previewOnly` keeps working, but `Some(false)` is ignored rather
/// than honored: any webview caller (DevTools, an injected script, a future
/// caller that copies the wrong example) could otherwise put a full page of
/// decrypted secrets in renderer memory, which is the SBS-912 leak. The
/// one-id full-body path is `get_clip_details`; `get_clips` keeps its own
/// back-compat default.
pub fn resolve_search_preview_only(_preview_only: Option<bool>) -> bool {
    true
}

/// Details/reveal always ships the full decrypted payload (or the surviving
/// image thumbnail). It does not honor `preview_only`.
pub fn details_item_content(clip_type: &str, decrypted: &[u8]) -> String {
    if clip_type == "image" {
        BASE64.encode(decrypted)
    } else {
        String::from_utf8_lossy(decrypted).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "UNIQUE-SBS-829-SECRET-TOKEN-8821-DO-NOT-SHIP";

    fn dump_body() -> String {
        format!("{}{SECRET}", "copied log line\n".repeat(200))
    }

    #[test]
    fn preview_only_text_withholds_the_full_decrypted_body() {
        let body = dump_body();
        assert!(body.len() > 2000);

        let content = list_item_content("text", body.as_bytes(), true, false);
        assert!(
            content.is_empty(),
            "preview_only text rows must not ship the decrypted body"
        );
        assert!(!content.contains(SECRET));

        let preview = list_item_preview("copied log line", false);
        assert_eq!(preview, "copied log line");
        assert!(!preview.contains(SECRET));
    }

    #[test]
    fn full_list_and_omitted_preview_only_still_ship_the_body() {
        let body = dump_body();
        let full = list_item_content("text", body.as_bytes(), false, false);
        assert_eq!(full, body);
        assert!(full.contains(SECRET));
    }

    #[test]
    fn hidden_rows_stay_blank_even_when_preview_only_is_false() {
        let body = dump_body();
        assert!(list_item_content("text", body.as_bytes(), false, true).is_empty());
        assert!(list_item_content("text", body.as_bytes(), true, true).is_empty());
        assert!(list_item_content("image", b"thumb-png-bytes", true, true).is_empty());
        assert!(list_item_preview("copied log line", true).is_empty());
        assert!(list_item_notes(Some("AWS root password"), true).is_none());
    }

    #[test]
    fn visible_notes_still_ship_on_the_list_row() {
        assert_eq!(
            list_item_notes(Some("meeting notes"), false).as_deref(),
            Some("meeting notes")
        );
        assert_eq!(list_item_notes(Some(""), false).as_deref(), Some(""));
        assert!(list_item_notes(None, false).is_none());
    }

    #[test]
    fn image_rows_still_ship_the_thumbnail_when_preview_only() {
        let thumb = b"thumb-png-bytes";
        let content = list_item_content("image", thumb, true, false);
        assert_eq!(content, BASE64.encode(thumb));
        assert!(!content.is_empty());
    }

    #[test]
    fn details_still_return_the_full_decrypted_body() {
        let body = dump_body();
        let details = details_item_content("text", body.as_bytes());
        assert_eq!(details, body);
        assert!(details.contains(SECRET));

        let thumb = b"thumb-png-bytes";
        assert_eq!(details_item_content("image", thumb), BASE64.encode(thumb));
    }

    /// Search path: a >2 KB dump with a unique suffix must not appear in
    /// `content` when the flag is true or omitted. This is the mapping
    /// `search_clips` uses (SBS-912). The in-crate `search_clips_in_database`
    /// test covers the IPC wiring on Windows CI.
    #[test]
    fn preview_only_search_withholds_full_text() {
        let body = dump_body();
        assert!(body.len() > 2000);

        for requested in [Some(true), None, Some(false)] {
            let preview_only = resolve_search_preview_only(requested);
            let content = list_item_content("text", body.as_bytes(), preview_only, false);
            assert!(
                content.is_empty(),
                "search text rows must not ship the decrypted body when preview_only={requested:?}"
            );
            assert!(
                !content.contains(SECRET),
                "the unique secret must not appear in search content"
            );
            let preview = list_item_preview("copied log line", false);
            assert_eq!(preview, "copied log line");
            assert!(!preview.contains(SECRET));
        }

        // Explicitly asking for full bodies does not get them. Search has no
        // caller that needs one, and honoring the opt-out is the leak.
        let opted_out = list_item_content(
            "text",
            body.as_bytes(),
            resolve_search_preview_only(Some(false)),
            false,
        );
        assert!(
            opted_out.is_empty(),
            "search must withhold the body even when previewOnly: false is requested"
        );
        assert!(!opted_out.contains(SECRET));

        let thumb = b"thumb-png-bytes";
        let image = list_item_content("image", thumb, resolve_search_preview_only(None), false);
        assert_eq!(image, BASE64.encode(thumb));

        assert!(list_item_content(
            "text",
            body.as_bytes(),
            resolve_search_preview_only(None),
            true
        )
        .is_empty());
        assert!(list_item_preview("copied log line", true).is_empty());
    }
}
