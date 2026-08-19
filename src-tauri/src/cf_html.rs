//! CF_HTML assembly and capture parsing.
//!
//! Capture stores the *document* part of a CF_HTML payload (the StartHTML..EndHTML
//! slice, header stripped). Writing that document back raw produces an invalid
//! "HTML Format" entry that Office-class apps reject, so every restore must
//! re-attach a header with correct byte offsets.
//!
//! Restores must hash the exact bytes a re-capture of our own write will read
//! back, so [`document`] (the normalized StartHTML..EndHTML slice) feeds the
//! ignore-hash material and [`to_cf_html`] produces the clipboard payload.
//!
//! Capture must not call clipboard-rs `get_html`: that helper slices the
//! StartHTML..EndHTML range with `str[]` after only checking that
//! `end - start <= len`, so a header whose `EndHTML` is past the payload
//! panics. Release builds `panic = "abort"`, so that kills the process
//! (SBS-999). [`html_document_from_cf_html`] reads raw bytes and uses `get`.

const START_FRAGMENT_MARKER: &str = "<!--StartFragment-->";
const END_FRAGMENT_MARKER: &str = "<!--EndFragment-->";

/// True when the payload already carries a CF_HTML header. Stored rows never
/// should, but never double-wrap if one slips through.
fn is_cf_html(payload: &str) -> bool {
    payload.starts_with("Version:") && payload.contains("StartHTML:")
}

/// Document slice from a raw CF_HTML clipboard payload.
///
/// Offsets are byte indexes from the start of the payload. Out-of-range,
/// inverted, or non-UTF-8 ranges are discarded instead of sliced.
pub(crate) fn html_document_from_cf_html(payload: &[u8]) -> Option<String> {
    let start = cf_html_offset(payload, "StartHTML").unwrap_or(0);
    let end = cf_html_offset(payload, "EndHTML").unwrap_or(payload.len());
    let document = payload.get(start..end)?;
    String::from_utf8(document.to_vec()).ok()
}

fn cf_html_offset(payload: &[u8], key: &str) -> Option<usize> {
    let header_end = payload
        .iter()
        .position(|&byte| byte == b'<')
        .unwrap_or(payload.len());
    let header = std::str::from_utf8(payload.get(..header_end)?).ok()?;
    for line in header.lines() {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name != key {
            continue;
        }
        let digits = value.trim();
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        return digits.parse().ok();
    }
    None
}

/// Normalize stored HTML into the document that will sit between StartHTML and
/// EndHTML. HTML that already looks like a document (fragment markers or an
/// `<html>` root) passes through byte-for-byte; bare fragments get the standard
/// container so fragment offsets exist.
pub(crate) fn document(html: &str) -> String {
    if is_cf_html(html) {
        if let Some(document_start) = html.find('<') {
            return html[document_start..].to_string();
        }
        return html.to_string();
    }
    if html.contains(START_FRAGMENT_MARKER)
        || html
            .trim_start()
            .get(..5)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("<html"))
    {
        return html.to_string();
    }
    format!(
        "<html>\r\n<body>\r\n{START_FRAGMENT_MARKER}{html}{END_FRAGMENT_MARKER}\r\n</body>\r\n</html>"
    )
}

/// Wrap HTML into a complete CF_HTML payload (Version/StartHTML/EndHTML/
/// StartFragment/EndFragment with zero-padded byte offsets).
pub(crate) fn to_cf_html(html: &str) -> String {
    if is_cf_html(html) {
        return html.to_string();
    }
    let document = document(html);

    // Fixed-width offsets keep the header length independent of the values.
    const PLACEHOLDER: &str = "0000000000";
    let header_len = format!(
        "Version:0.9\r\nStartHTML:{PLACEHOLDER}\r\nEndHTML:{PLACEHOLDER}\r\nStartFragment:{PLACEHOLDER}\r\nEndFragment:{PLACEHOLDER}\r\n"
    )
    .len();

    let start_html = header_len;
    let end_html = header_len + document.len();
    let start_fragment = header_len
        + document
            .find(START_FRAGMENT_MARKER)
            .map(|position| position + START_FRAGMENT_MARKER.len())
            .unwrap_or(0);
    let end_fragment = header_len
        + document
            .rfind(END_FRAGMENT_MARKER)
            .unwrap_or(document.len());

    format!(
        "Version:0.9\r\nStartHTML:{start_html:010}\r\nEndHTML:{end_html:010}\r\nStartFragment:{start_fragment:010}\r\nEndFragment:{end_fragment:010}\r\n{document}"
    )
}

#[cfg(test)]
mod tests {
    use super::{document, html_document_from_cf_html, to_cf_html};

    fn header_offset(payload: &str, key: &str) -> usize {
        payload
            .lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))
            .expect("header present")
            .parse()
            .expect("numeric offset")
    }

    #[test]
    fn wraps_bare_fragment_with_valid_offsets() {
        let fragment = "<b>héllo</b>";
        let payload = to_cf_html(fragment);

        let start_html = header_offset(&payload, "StartHTML");
        let end_html = header_offset(&payload, "EndHTML");
        let start_fragment = header_offset(&payload, "StartFragment");
        let end_fragment = header_offset(&payload, "EndFragment");

        assert!(payload.as_bytes()[start_html..].starts_with(b"<html>"));
        assert_eq!(end_html, payload.len());
        assert_eq!(&payload[start_fragment..end_fragment], fragment);
    }

    #[test]
    fn preserves_captured_document_bytes() {
        // What get_html hands back for a typical browser copy: full document
        // including fragment markers, header already stripped.
        let captured =
            "<html>\r\n<body>\r\n<!--StartFragment--><p>from chrome</p><!--EndFragment-->\r\n</body>\r\n</html>";
        assert_eq!(document(captured), captured);

        let payload = to_cf_html(captured);
        let start_html = header_offset(&payload, "StartHTML");
        assert_eq!(&payload[start_html..], captured);
        let start_fragment = header_offset(&payload, "StartFragment");
        let end_fragment = header_offset(&payload, "EndFragment");
        assert_eq!(&payload[start_fragment..end_fragment], "<p>from chrome</p>");
    }

    #[test]
    fn does_not_double_wrap_existing_cf_html() {
        let existing = to_cf_html("<i>once</i>");
        assert_eq!(to_cf_html(&existing), existing);
        assert_eq!(document(&existing), document("<i>once</i>"));
    }

    #[test]
    fn document_without_markers_keeps_html_root() {
        let doc = "<HTML><body><p>no markers</p></body></HTML>";
        assert_eq!(document(doc), doc);

        let payload = to_cf_html(doc);
        let start_fragment = header_offset(&payload, "StartFragment");
        let end_fragment = header_offset(&payload, "EndFragment");
        // No markers: the whole document is the fragment.
        assert_eq!(&payload[start_fragment..end_fragment], doc);
    }

    #[test]
    fn html_document_from_cf_html_reads_a_well_formed_payload() {
        let fragment = "<b>héllo</b>";
        let payload = to_cf_html(fragment);
        let parsed = html_document_from_cf_html(payload.as_bytes()).expect("valid CF_HTML");
        assert_eq!(parsed, document(fragment));
    }

    #[test]
    fn html_document_from_cf_html_rejects_the_sbs_999_oversize_end_offset() {
        // Difference EndHTML-StartHTML is 60, payload is 83 bytes: clipboard-rs
        // 0.2.4 treats that as valid, then panics on data[60..120].
        let payload = concat!(
            "Version:0.9\r\n",
            "StartHTML:0000000060\r\n",
            "EndHTML:0000000120\r\n",
            "<html><body>hi</body></html>"
        );
        assert_eq!(payload.len(), 83);
        assert!(html_document_from_cf_html(payload.as_bytes()).is_none());
    }

    #[test]
    fn html_document_from_cf_html_rejects_inverted_offsets() {
        let payload = concat!(
            "Version:0.9\r\n",
            "StartHTML:0000000080\r\n",
            "EndHTML:0000000040\r\n",
            "<html></html>"
        );
        assert!(html_document_from_cf_html(payload.as_bytes()).is_none());
    }

    #[test]
    fn html_document_from_cf_html_rejects_a_slice_that_is_not_utf8() {
        let header = "Version:0.9\r\nStartHTML:0000000055\r\nEndHTML:0000000057\r\n";
        assert_eq!(header.len(), 55);
        let mut payload = header.as_bytes().to_vec();
        payload.extend_from_slice(&[0x80, 0x80]);
        assert!(html_document_from_cf_html(&payload).is_none());
    }
}
