//! Classify a clipboard miss when Unicode text is empty or absent.
//!
//! SBS-924: an empty `CF_UNICODETEXT` payload is not a clipboard lock. Capture
//! still keeps HTML/RTF when they have content. Empty text plus only private
//! formats is unsupported (handled), not contention. A clipboard we never
//! opened stays its own state: a real lock.
//!
//! This module has no crate dependencies so `rustc --test` can run it on a
//! Linux box that cannot compile the Windows crate.

/// What we learned about one clipboard format after trying to read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PayloadRead<'a> {
    /// Successfully read. `""` is a real empty payload, not a lock.
    Present(&'a str),
    /// Format was not available, or `get_*` returned an error.
    Missing,
    /// We never observed this format (clipboard never opened, or not asked).
    Unknown,
}

/// What to do with HTML/RTF after reading Unicode text (which may be empty).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RichCapture {
    pub searchable_text: String,
    pub html: Option<String>,
    pub rtf: Option<String>,
}

/// After materialize has no content, why — and what the capture path should do.
///
/// Three states, not two: empty text is not a lock, and a missing format is
/// not empty text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MissAction {
    /// HTML or RTF has content. Store that clip; do not diagnose a lock.
    CaptureRich,
    /// Placeholder-only empty clipboard. Password-manager auto-clear.
    Cleared,
    /// Empty or missing text, no rich payload, no image. Private/custom only
    /// or nothing we store. Mark handled; do not restart the listener.
    Unsupported,
    /// Could not open the clipboard, or a supported payload stayed unreadable.
    Contended,
}

/// Build a storable clip from observed text/HTML/RTF.
///
/// Non-empty Unicode text wins as the searchable body (existing path). Empty
/// or missing Unicode text still yields a clip when HTML or RTF has content;
/// the searchable body is derived from the rich payload so History has
/// something to show and search.
pub(crate) fn rich_capture_from_payloads(
    text: PayloadRead<'_>,
    html: PayloadRead<'_>,
    rtf: PayloadRead<'_>,
) -> Option<RichCapture> {
    let html = nonempty_present(html);
    let rtf = nonempty_present(rtf);

    if let PayloadRead::Present(body) = text {
        if !body.is_empty() {
            return Some(RichCapture {
                searchable_text: body.to_string(),
                html: html.map(str::to_string),
                rtf: rtf.map(str::to_string),
            });
        }
    }

    if html.is_none() && rtf.is_none() {
        return None;
    }

    let searchable_text = html
        .map(plain_text_from_html)
        .filter(|body| !body.is_empty())
        .or_else(|| rtf.map(plain_text_from_rtf).filter(|body| !body.is_empty()))
        .unwrap_or_else(|| {
            if html.is_some() {
                "[HTML]".to_string()
            } else {
                "[RTF]".to_string()
            }
        });

    Some(RichCapture {
        searchable_text,
        html: html.map(str::to_string),
        rtf: rtf.map(str::to_string),
    })
}

/// Decide the post-miss action from observed facts.
///
/// `only_placeholder_text_formats` is `None` when we could not enumerate
/// formats (unknown). That must not become a clear (would delete the previous
/// capture) or a lock (the SBS-924 false diagnosis).
pub(crate) fn classify_miss(
    text: PayloadRead<'_>,
    html: PayloadRead<'_>,
    rtf: PayloadRead<'_>,
    image_advertised: bool,
    only_placeholder_text_formats: Option<bool>,
    clipboard_opened: bool,
) -> MissAction {
    if rich_capture_from_payloads(text, html, rtf).is_some() {
        // Non-empty text, or empty/missing text with HTML/RTF. Neither is a lock.
        return MissAction::CaptureRich;
    }

    // Advertised but unread text/HTML/RTF is delayed render or a lost read,
    // not "empty" and not "missing".
    if matches!(text, PayloadRead::Unknown)
        || matches!(html, PayloadRead::Unknown)
        || matches!(rtf, PayloadRead::Unknown)
    {
        return MissAction::Contended;
    }

    if !clipboard_opened
        || matches!(text, PayloadRead::Unknown)
            && matches!(html, PayloadRead::Unknown)
            && matches!(rtf, PayloadRead::Unknown)
    {
        return MissAction::Contended;
    }

    if image_advertised {
        return MissAction::Contended;
    }

    match only_placeholder_text_formats {
        Some(true) => MissAction::Cleared,
        Some(false) => MissAction::Unsupported,
        None => match text {
            // Known empty text, formats unknown: do not clear (might still
            // hold a private format) and do not lock (we already read).
            PayloadRead::Present("") => MissAction::Unsupported,
            PayloadRead::Present(_) => MissAction::Unsupported,
            PayloadRead::Missing => MissAction::Unsupported,
            PayloadRead::Unknown => MissAction::Contended,
        },
    }
}

fn nonempty_present(read: PayloadRead<'_>) -> Option<&str> {
    match read {
        PayloadRead::Present(body) if !body.is_empty() => Some(body),
        _ => None,
    }
}

/// Best-effort visible text from a CF_HTML document (header already stripped).
pub(crate) fn plain_text_from_html(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    let mut tag = String::new();
    for ch in html.chars() {
        match ch {
            '<' => {
                in_tag = true;
                tag.clear();
            }
            '>' if in_tag => {
                in_tag = false;
                let name = tag
                    .trim_start_matches('/')
                    .split(|c: char| c.is_ascii_whitespace() || c == '/')
                    .next()
                    .unwrap_or("");
                if matches!(
                    name.to_ascii_lowercase().as_str(),
                    "p" | "div" | "br" | "tr" | "li" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
                ) {
                    if !out.ends_with(' ') && !out.is_empty() {
                        out.push(' ');
                    }
                }
            }
            _ if in_tag => tag.push(ch),
            _ => out.push(ch),
        }
    }
    decode_basic_entities(&out)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Best-effort visible text from a simple RTF payload.
pub(crate) fn plain_text_from_rtf(rtf: &str) -> String {
    let bytes = rtf.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                i += 1;
                if i >= bytes.len() {
                    break;
                }
                match bytes[i] {
                    b'\\' | b'{' | b'}' => {
                        out.push(bytes[i] as char);
                        i += 1;
                    }
                    b'\'' => {
                        if i + 2 < bytes.len() {
                            if let Ok(value) = u8::from_str_radix(&rtf[i + 1..i + 3], 16) {
                                if value >= 32 {
                                    out.push(value as char);
                                }
                            }
                            i += 3;
                        } else {
                            i += 1;
                        }
                    }
                    b'\n' | b'\r' => i += 1,
                    c if c.is_ascii_alphabetic() => {
                        i += 1;
                        while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                            i += 1;
                        }
                        if i < bytes.len() && (bytes[i] == b'-' || bytes[i].is_ascii_digit()) {
                            if bytes[i] == b'-' {
                                i += 1;
                            }
                            while i < bytes.len() && bytes[i].is_ascii_digit() {
                                i += 1;
                            }
                        }
                        if i < bytes.len() && bytes[i] == b' ' {
                            i += 1;
                        }
                    }
                    _ => i += 1,
                }
            }
            b'{' | b'}' | b'\n' | b'\r' => i += 1,
            c => {
                out.push(c as char);
                i += 1;
            }
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode_basic_entities(input: &str) -> String {
    input
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

#[cfg(test)]
mod tests {
    use super::{
        classify_miss, plain_text_from_html, plain_text_from_rtf, rich_capture_from_payloads,
        MissAction, PayloadRead,
    };

    /// SBS-924: empty CF_UNICODETEXT plus live HTML is a rich capture, not a lock.
    #[test]
    fn empty_text_plus_html_is_captured_not_locked() {
        let html = "<b>Office format only</b>";
        let captured = rich_capture_from_payloads(
            PayloadRead::Present(""),
            PayloadRead::Present(html),
            PayloadRead::Missing,
        )
        .expect("empty Unicode text must not drop HTML");
        assert_eq!(captured.searchable_text, "Office format only");
        assert_eq!(captured.html.as_deref(), Some(html));
        assert_eq!(captured.rtf, None);
        assert_eq!(
            classify_miss(
                PayloadRead::Present(""),
                PayloadRead::Present(html),
                PayloadRead::Missing,
                false,
                Some(false),
                true,
            ),
            MissAction::CaptureRich
        );
    }

    /// SBS-924: empty text plus a private/custom format is handled, not a lock.
    #[test]
    fn empty_text_plus_private_format_is_unsupported_not_locked() {
        assert!(rich_capture_from_payloads(
            PayloadRead::Present(""),
            PayloadRead::Missing,
            PayloadRead::Missing,
        )
        .is_none());
        assert_eq!(
            classify_miss(
                PayloadRead::Present(""),
                PayloadRead::Missing,
                PayloadRead::Missing,
                false,
                Some(false),
                true,
            ),
            MissAction::Unsupported
        );
    }

    /// SBS-924: placeholder-only empty clipboard is a clear, not a lock.
    #[test]
    fn truly_empty_clipboard_is_cleared_not_locked() {
        assert_eq!(
            classify_miss(
                PayloadRead::Present(""),
                PayloadRead::Missing,
                PayloadRead::Missing,
                false,
                Some(true),
                true,
            ),
            MissAction::Cleared
        );
    }

    /// A clipboard we never opened is still a lock, even if text *might* be empty.
    #[test]
    fn unopened_clipboard_is_still_a_lock() {
        assert_eq!(
            classify_miss(
                PayloadRead::Unknown,
                PayloadRead::Unknown,
                PayloadRead::Unknown,
                false,
                None,
                false,
            ),
            MissAction::Contended
        );
    }

    #[test]
    fn empty_text_plus_rtf_is_captured() {
        let rtf = r"{\rtf1\ansi Hello from Word}";
        let captured = rich_capture_from_payloads(
            PayloadRead::Present(""),
            PayloadRead::Missing,
            PayloadRead::Present(rtf),
        )
        .expect("empty Unicode text must not drop RTF");
        assert_eq!(captured.searchable_text, "Hello from Word");
        assert_eq!(captured.rtf.as_deref(), Some(rtf));
    }

    #[test]
    fn missing_text_is_not_collapsed_into_empty_text() {
        // get_text Err is Missing, not Present(""). HTML still wins.
        let captured = rich_capture_from_payloads(
            PayloadRead::Missing,
            PayloadRead::Present("<p>still here</p>"),
            PayloadRead::Missing,
        )
        .expect("a missing Unicode format must not drop HTML");
        assert_eq!(captured.searchable_text, "still here");

        // Missing text, no rich, no image, private formats: unsupported, not lock.
        assert_eq!(
            classify_miss(
                PayloadRead::Missing,
                PayloadRead::Missing,
                PayloadRead::Missing,
                false,
                Some(false),
                true,
            ),
            MissAction::Unsupported
        );
    }

    #[test]
    fn unread_advertised_unicode_text_is_contention() {
        assert_eq!(
            classify_miss(
                PayloadRead::Unknown,
                PayloadRead::Missing,
                PayloadRead::Missing,
                false,
                Some(false),
                true,
            ),
            MissAction::Contended
        );
    }

    /// Advertised HTML that has not rendered yet is unknown, not "no HTML".
    #[test]
    fn empty_text_plus_unread_html_is_contention_not_unsupported() {
        assert_eq!(
            classify_miss(
                PayloadRead::Present(""),
                PayloadRead::Unknown,
                PayloadRead::Missing,
                false,
                Some(false),
                true,
            ),
            MissAction::Contended
        );
    }

    #[test]
    fn advertised_unread_image_is_contention() {
        assert_eq!(
            classify_miss(
                PayloadRead::Present(""),
                PayloadRead::Missing,
                PayloadRead::Missing,
                true,
                Some(false),
                true,
            ),
            MissAction::Contended
        );
    }

    #[test]
    fn unknown_format_enum_after_empty_text_is_not_a_lock_or_a_clear() {
        assert_eq!(
            classify_miss(
                PayloadRead::Present(""),
                PayloadRead::Missing,
                PayloadRead::Missing,
                false,
                None,
                true,
            ),
            MissAction::Unsupported
        );
    }

    #[test]
    fn nonempty_unicode_text_keeps_html_and_rtf_companions() {
        let captured = rich_capture_from_payloads(
            PayloadRead::Present("visible"),
            PayloadRead::Present("<i>visible</i>"),
            PayloadRead::Present(r"{\rtf1 visible}"),
        )
        .expect("non-empty text is still captured");
        assert_eq!(captured.searchable_text, "visible");
        assert_eq!(captured.html.as_deref(), Some("<i>visible</i>"));
        assert_eq!(captured.rtf.as_deref(), Some(r"{\rtf1 visible}"));
    }

    #[test]
    fn empty_html_and_rtf_are_not_content() {
        assert!(rich_capture_from_payloads(
            PayloadRead::Present(""),
            PayloadRead::Present(""),
            PayloadRead::Present(""),
        )
        .is_none());
    }

    #[test]
    fn html_entities_and_tags_decode_to_searchable_text() {
        assert_eq!(
            plain_text_from_html("<div>A&nbsp;&amp;&nbsp;B</div>"),
            "A & B"
        );
    }

    #[test]
    fn rtf_control_words_are_stripped() {
        assert_eq!(
            plain_text_from_rtf(r"{\rtf1\ansi\deff0 {\fonttbl} Hello}"),
            "Hello"
        );
    }
}
