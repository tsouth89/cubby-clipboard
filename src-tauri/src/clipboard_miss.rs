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

/// Everything one materialize attempt learned before it decides what to do.
///
/// Kept as one struct so production and the tests below feed [`decide_capture`]
/// the same facts. The decision used to live inline in
/// `materialize_clipboard_content_once`, where no test could reach it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AttemptFacts<'a> {
    pub text: PayloadRead<'a>,
    pub html: PayloadRead<'a>,
    pub rtf: PayloadRead<'a>,
    /// An image format is advertised on the clipboard.
    pub image_advertised: bool,
    /// An image actually decoded on this attempt.
    pub image_readable: bool,
    /// Last of the bounded attempts: nothing will be retried after this one.
    pub last_attempt: bool,
}

/// What the caller should do with one materialize attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CaptureDecision {
    /// Store the image that already decoded.
    Image,
    /// Store this text / HTML / RTF clip.
    Rich(RichCapture),
    /// Nothing to store and nothing left to wait for. Not a lock: the caller
    /// marks the sequence handled (or cleared) instead of restarting the
    /// listener (SBS-924).
    DeterminateMiss,
    /// Try again while attempts remain.
    Transient,
}

/// Decide one attempt from what it observed.
///
/// Order is the whole point:
/// 1. Non-empty Unicode text is the copy. HTML and RTF ride along.
/// 2. Unicode text that is advertised but unread is a delayed render, so retry
///    while attempts remain rather than storing an HTML-derived stand-in for a
///    body that is about to arrive.
/// 3. A readable image beats an HTML/RTF-derived body. A Word, PowerPoint, or
///    Outlook copy of a picture has empty Unicode text, an HTML wrapper, RTF,
///    and a bitmap; storing the wrapper would lose the picture entirely.
/// 4. An advertised-but-unread image still retries before HTML/RTF, except on
///    the last attempt. The first fast PNG read often misses, and accepting
///    the wrapper then stops later attempts from ever seeing the bitmap.
/// 5. Otherwise HTML or RTF with content is the clip.
/// 6. Unread HTML/RTF, or a last-attempt unread image, is still contention.
pub(crate) fn decide_capture(facts: AttemptFacts<'_>) -> CaptureDecision {
    let html = nonempty_present(facts.html);
    let rtf = nonempty_present(facts.rtf);

    if let PayloadRead::Present(body) = facts.text {
        if !body.is_empty() {
            return CaptureDecision::Rich(RichCapture {
                searchable_text: body.to_string(),
                html: html.map(str::to_string),
                rtf: rtf.map(str::to_string),
            });
        }
    }

    if matches!(facts.text, PayloadRead::Unknown) && !facts.last_attempt {
        return CaptureDecision::Transient;
    }

    if facts.image_readable {
        return CaptureDecision::Image;
    }

    if facts.image_advertised && !facts.last_attempt {
        return CaptureDecision::Transient;
    }

    if let Some(rich) = rich_capture_from_payloads(facts.text, facts.html, facts.rtf) {
        return CaptureDecision::Rich(rich);
    }

    if facts.image_advertised
        || matches!(facts.text, PayloadRead::Unknown)
        || matches!(facts.html, PayloadRead::Unknown)
        || matches!(facts.rtf, PayloadRead::Unknown)
    {
        return CaptureDecision::Transient;
    }

    CaptureDecision::DeterminateMiss
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

fn nonempty_present(read: PayloadRead<'_>) -> Option<&str> {
    match read {
        PayloadRead::Present(body) if !body.is_empty() => Some(body),
        _ => None,
    }
}

/// Elements whose text is never the copied content. `xml` and the Office
/// conditional comments around it carry Word settings such as
/// `<w:View>Normal</w:View>`, which used to land in the stored clip.
const NON_CONTENT_ELEMENTS: [&str; 5] = ["head", "style", "script", "xml", "title"];

/// Elements that end a visible run. Whitespace is collapsed at the end, so
/// emitting a plain space here is enough.
const BREAKING_ELEMENTS: [&str; 13] = [
    "p",
    "div",
    "br",
    "tr",
    "li",
    "td",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "blockquote",
];

/// Best-effort visible text from a CF_HTML document (header already stripped).
///
/// `get_html` returns the whole StartHTML..EndHTML document, so this has to
/// ignore comments, non-content elements, and everything outside the
/// StartFragment..EndFragment markers when the source wrote them.
pub(crate) fn plain_text_from_html(html: &str) -> String {
    let chars: Vec<char> = html_fragment(html).chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '<' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        // A comment runs to the first `-->`, which for an Office conditional
        // block is after the `<![endif]`, so the Word XML inside goes with it.
        if starts_with(&chars, i, "<!--") {
            i = match find_from(&chars, i + 4, "-->") {
                Some(end) => end + 3,
                None => chars.len(),
            };
            continue;
        }
        let Some(close) = chars[i..].iter().position(|c| *c == '>').map(|at| i + at) else {
            // Unterminated tag: nothing after it can be trusted as markup.
            break;
        };
        let raw: String = chars[i + 1..close].iter().collect();
        let name = tag_name(&raw);
        i = close + 1;
        let is_open = !raw.starts_with('/') && !raw.ends_with('/');
        if is_open && NON_CONTENT_ELEMENTS.contains(&name.as_str()) {
            i = skip_element(&chars, i, &name);
            continue;
        }
        if BREAKING_ELEMENTS.contains(&name.as_str()) {
            out.push(' ');
        }
    }
    decode_basic_entities(&out)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The copied selection, when the source marked it. Everything outside is
/// document scaffolding the user did not copy.
fn html_fragment(html: &str) -> &str {
    const START: &str = "<!--StartFragment-->";
    const END: &str = "<!--EndFragment-->";
    let Some(start) = html.find(START) else {
        return html;
    };
    let rest = &html[start + START.len()..];
    match rest.find(END) {
        Some(end) => &rest[..end],
        None => rest,
    }
}

fn tag_name(raw: &str) -> String {
    raw.trim_start_matches('/')
        .split(|c: char| c.is_ascii_whitespace() || c == '/' || c == '>')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn starts_with(chars: &[char], at: usize, needle: &str) -> bool {
    needle
        .chars()
        .enumerate()
        .all(|(offset, expected)| chars.get(at + offset) == Some(&expected))
}

fn find_from(chars: &[char], from: usize, needle: &str) -> Option<usize> {
    (from..chars.len()).find(|at| starts_with(chars, *at, needle))
}

/// Index just past `</name>`, or the end of the document when it never closes.
fn skip_element(chars: &[char], from: usize, name: &str) -> usize {
    let closing = format!("</{name}");
    let mut i = from;
    while i < chars.len() {
        if chars[i] == '<' {
            let lowered: String = chars[i..(i + closing.len()).min(chars.len())]
                .iter()
                .collect::<String>()
                .to_ascii_lowercase();
            if lowered == closing {
                return match chars[i..].iter().position(|c| *c == '>') {
                    Some(at) => i + at + 1,
                    None => chars.len(),
                };
            }
        }
        i += 1;
    }
    chars.len()
}

/// Destination groups whose contents are binary or table data, not the copied
/// text. `\pict` is the one that matters most: its hex image bytes used to be
/// copied straight into the stored clip.
const RTF_NON_CONTENT_DESTINATIONS: [&str; 8] = [
    "pict",
    "fonttbl",
    "colortbl",
    "stylesheet",
    "info",
    "header",
    "footer",
    "themedata",
];

/// Best-effort visible text from a simple RTF payload.
pub(crate) fn plain_text_from_rtf(rtf: &str) -> String {
    let bytes = rtf.as_bytes();
    let mut out = String::new();
    let mut depth: usize = 0;
    // Depth of the outermost group being skipped, if any.
    let mut skipping_from: Option<usize> = None;
    // `\ucN` fallback length after `\u`; RTF default is one ANSI byte.
    let mut ansi_fallback_bytes: usize = 1;
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => {
                depth += 1;
                i += 1;
            }
            b'}' => {
                if skipping_from == Some(depth) {
                    skipping_from = None;
                }
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b'\\' => {
                i += 1;
                if i >= bytes.len() {
                    break;
                }
                match bytes[i] {
                    b'\\' | b'{' | b'}' => {
                        if skipping_from.is_none() {
                            out.push(bytes[i] as char);
                        }
                        i += 1;
                    }
                    b'\'' => {
                        if i + 2 < bytes.len() {
                            if let Ok(value) = u8::from_str_radix(&rtf[i + 1..i + 3], 16) {
                                if skipping_from.is_none() && value >= 32 {
                                    out.push(value as char);
                                }
                            }
                            i += 3;
                        } else {
                            i += 1;
                        }
                    }
                    // `\*` marks an ignorable destination: generator strings,
                    // list tables, and similar. None of it is copied text.
                    b'*' => {
                        skipping_from.get_or_insert(depth);
                        i += 1;
                    }
                    b'\n' | b'\r' => i += 1,
                    c if c.is_ascii_alphabetic() => {
                        let start = i;
                        while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
                            i += 1;
                        }
                        let word = &rtf[start..i];
                        let mut param: Option<i32> = None;
                        if i < bytes.len() && (bytes[i] == b'-' || bytes[i].is_ascii_digit()) {
                            let param_start = i;
                            if bytes[i] == b'-' {
                                i += 1;
                            }
                            while i < bytes.len() && bytes[i].is_ascii_digit() {
                                i += 1;
                            }
                            param = rtf[param_start..i].parse().ok();
                        }
                        if word == "bin" {
                            // `\binN` is followed immediately by N raw bytes.
                            // Those bytes are not RTF tokens: a 0x7D in a
                            // Word `\pict\bin` payload would otherwise close
                            // the skip and leak the rest into clip text.
                            let skip = param.unwrap_or(0).max(0) as usize;
                            i = (i + skip).min(bytes.len());
                            continue;
                        }
                        // One trailing space is the control-word delimiter, not
                        // content. Not for `\bin`, whose payload starts here.
                        if i < bytes.len() && bytes[i] == b' ' {
                            i += 1;
                        }
                        if word == "uc" {
                            if let Some(count) = param {
                                ansi_fallback_bytes = count.max(0) as usize;
                            }
                        } else if word == "u" {
                            if skipping_from.is_none() {
                                if let Some(code) = param {
                                    let unsigned = if code < 0 {
                                        code.wrapping_add(65536)
                                    } else {
                                        code
                                    } as u32;
                                    if let Some(ch) = char::from_u32(unsigned) {
                                        out.push(ch);
                                    }
                                }
                            }
                            i = (i + ansi_fallback_bytes).min(bytes.len());
                        } else if RTF_NON_CONTENT_DESTINATIONS.contains(&word) {
                            skipping_from.get_or_insert(depth);
                        } else if skipping_from.is_none()
                            && matches!(word, "par" | "line" | "tab" | "cell" | "row" | "sect")
                        {
                            // Word and Outlook break paragraphs with `\par`. No
                            // space here concatenates adjacent paragraphs, so
                            // search stops matching on word boundaries.
                            out.push(' ');
                        }
                    }
                    _ => i += 1,
                }
            }
            b'\n' | b'\r' => i += 1,
            c => {
                if skipping_from.is_none() {
                    out.push(c as char);
                }
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
        decide_capture, plain_text_from_html, plain_text_from_rtf, rich_capture_from_payloads,
        AttemptFacts, CaptureDecision, PayloadRead,
    };

    /// Facts for a clipboard with no image and no attempts left to spend, which
    /// is the shape most of these cases care about.
    fn facts<'a>(
        text: PayloadRead<'a>,
        html: PayloadRead<'a>,
        rtf: PayloadRead<'a>,
    ) -> AttemptFacts<'a> {
        AttemptFacts {
            text,
            html,
            rtf,
            image_advertised: false,
            image_readable: false,
            last_attempt: false,
        }
    }

    fn rich_text(decision: &CaptureDecision) -> &str {
        match decision {
            CaptureDecision::Rich(rich) => &rich.searchable_text,
            other => panic!("expected a rich capture, got {other:?}"),
        }
    }

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

        let decision = decide_capture(facts(
            PayloadRead::Present(""),
            PayloadRead::Present(html),
            PayloadRead::Missing,
        ));
        assert_eq!(rich_text(&decision), "Office format only");
    }

    /// SBS-924 follow-up: a Word, PowerPoint, or Outlook copy of a picture has
    /// empty Unicode text, an HTML wrapper, RTF, and a bitmap. Storing the
    /// wrapper as a text clip loses the picture, which is never sent on the
    /// content event and so never reaches the clips table.
    #[test]
    fn a_readable_image_beats_an_html_derived_body() {
        let decision = decide_capture(AttemptFacts {
            text: PayloadRead::Present(""),
            html: PayloadRead::Present("<img src=\"file:///C:/Temp/image001.png\">"),
            rtf: PayloadRead::Present(r"{\rtf1{\pict\pngblip 89504e47}}"),
            image_advertised: true,
            image_readable: true,
            last_attempt: false,
        });
        assert_eq!(decision, CaptureDecision::Image);
    }

    /// Real text still wins over a bitmap that happens to ride along.
    #[test]
    fn readable_text_still_beats_an_image() {
        let decision = decide_capture(AttemptFacts {
            text: PayloadRead::Present("the copied phrase"),
            html: PayloadRead::Present("<p>the copied phrase</p>"),
            rtf: PayloadRead::Missing,
            image_advertised: true,
            image_readable: true,
            last_attempt: false,
        });
        assert_eq!(rich_text(&decision), "the copied phrase");
    }

    /// Advertised CF_UNICODETEXT that has not rendered yet is a delayed read,
    /// not an empty body. Accepting an HTML-derived stand-in on the first try
    /// stores that stand-in forever: the sequence is noted as handled and the
    /// real text is never read again.
    #[test]
    fn unread_advertised_text_retries_before_accepting_html() {
        let attempt = AttemptFacts {
            text: PayloadRead::Unknown,
            html: PayloadRead::Present("<p>wrapper</p>"),
            rtf: PayloadRead::Missing,
            image_advertised: false,
            image_readable: false,
            last_attempt: false,
        };
        assert_eq!(decide_capture(attempt), CaptureDecision::Transient);

        // Out of attempts, the wrapper is better than nothing.
        let last = AttemptFacts {
            last_attempt: true,
            ..attempt
        };
        assert_eq!(rich_text(&decide_capture(last)), "wrapper");
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
            decide_capture(facts(
                PayloadRead::Present(""),
                PayloadRead::Missing,
                PayloadRead::Missing,
            )),
            CaptureDecision::DeterminateMiss
        );
    }

    /// A clipboard we never opened is still a lock, even if text *might* be empty.
    #[test]
    fn unopened_clipboard_is_still_a_lock() {
        assert_eq!(
            decide_capture(facts(
                PayloadRead::Unknown,
                PayloadRead::Unknown,
                PayloadRead::Unknown,
            )),
            CaptureDecision::Transient
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
        // get_text Err with the format not advertised is Missing, not
        // Present(""). HTML still wins, and there is nothing to wait for.
        let captured = rich_capture_from_payloads(
            PayloadRead::Missing,
            PayloadRead::Present("<p>still here</p>"),
            PayloadRead::Missing,
        )
        .expect("a missing Unicode format must not drop HTML");
        assert_eq!(captured.searchable_text, "still here");

        assert_eq!(
            decide_capture(facts(
                PayloadRead::Missing,
                PayloadRead::Missing,
                PayloadRead::Missing,
            )),
            CaptureDecision::DeterminateMiss
        );
    }

    /// Advertised HTML that has not rendered yet is unknown, not "no HTML".
    #[test]
    fn empty_text_plus_unread_html_is_contention_not_unsupported() {
        assert_eq!(
            decide_capture(facts(
                PayloadRead::Present(""),
                PayloadRead::Unknown,
                PayloadRead::Missing,
            )),
            CaptureDecision::Transient
        );
    }

    #[test]
    fn advertised_unread_image_is_contention() {
        assert_eq!(
            decide_capture(AttemptFacts {
                text: PayloadRead::Present(""),
                html: PayloadRead::Missing,
                rtf: PayloadRead::Missing,
                image_advertised: true,
                image_readable: false,
                last_attempt: false,
            }),
            CaptureDecision::Transient
        );
    }

    /// Office picture copies advertise PNG/DIB plus an HTML wrapper. The first
    /// fast image read often misses; accepting the wrapper then marks the
    /// sequence handled and the bitmap never reaches history.
    #[test]
    fn unread_advertised_image_retries_before_accepting_html() {
        let attempt = AttemptFacts {
            text: PayloadRead::Present(""),
            html: PayloadRead::Present("<img src=\"file:///C:/Temp/image001.png\">"),
            rtf: PayloadRead::Present(r"{\rtf1{\pict\pngblip 89504e47}}"),
            image_advertised: true,
            image_readable: false,
            last_attempt: false,
        };
        assert_eq!(decide_capture(attempt), CaptureDecision::Transient);

        let last = AttemptFacts {
            last_attempt: true,
            ..attempt
        };
        let decision = decide_capture(last);
        assert!(
            matches!(decision, CaptureDecision::Rich(_)),
            "last attempt should keep the wrapper rather than miss: {decision:?}"
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

    /// `get_html` hands back the whole StartHTML..EndHTML document. Word's
    /// conditional comments hold settings markup such as
    /// `<w:View>Normal</w:View>`, which used to be copied into the stored clip
    /// and pasted instead of the copied phrase.
    #[test]
    fn office_conditional_comments_and_settings_are_not_clip_text() {
        let word = concat!(
            "<html><head><style>p { margin: 0 }</style>",
            "<!--[if gte mso 9]><xml><w:WordDocument><w:View>Normal</w:View>",
            "</w:WordDocument></xml><![endif]--></head>",
            "<body><p>Hello</p></body></html>"
        );
        assert_eq!(plain_text_from_html(word), "Hello");
    }

    /// When the source marks the selection, only the selection is the copy.
    #[test]
    fn only_the_marked_fragment_is_clip_text() {
        let document = concat!(
            "<html><body>before<!--StartFragment--><p>the copied phrase</p>",
            "<!--EndFragment-->after</body></html>"
        );
        assert_eq!(plain_text_from_html(document), "the copied phrase");
    }

    /// A stylesheet in the body is markup, not content.
    #[test]
    fn style_and_script_bodies_are_not_clip_text() {
        assert_eq!(
            plain_text_from_html("<div><style>.a{color:red}</style>kept<script>x=1</script></div>"),
            "kept"
        );
    }

    #[test]
    fn rtf_control_words_are_stripped() {
        assert_eq!(
            plain_text_from_rtf(r"{\rtf1\ansi\deff0 {\fonttbl} Hello}"),
            "Hello"
        );
    }

    /// `\par` is how Word and Outlook break paragraphs. Dropping it entirely
    /// runs the paragraphs together, so search stops matching on the word
    /// boundary between them.
    #[test]
    fn rtf_paragraph_breaks_become_whitespace() {
        assert_eq!(
            plain_text_from_rtf(r"{\rtf1 Hello\par World}"),
            "Hello World"
        );
        // A control word ends at the first non-letter, so `\par` followed by a
        // newline is the other shape Word writes. `\parWorld` is a different
        // control word, not `\par` plus text, and is not RTF Word emits.
        assert_eq!(
            plain_text_from_rtf("{\\rtf1 Hello\\par\nWorld}"),
            "Hello World"
        );
        assert_eq!(plain_text_from_rtf(r"{\rtf1 a\tab b\line c}"), "a b c");
    }

    /// A pasted picture carries its bytes as hex inside `\pict`. Those used to
    /// be copied straight into the stored body and the History preview.
    #[test]
    fn rtf_picture_bytes_and_tables_are_not_clip_text() {
        let word = concat!(
            r"{\rtf1\ansi{\fonttbl{\f0 Calibri;}}{\colortbl;\red0\green0\blue0;}",
            r"{\*\generator Riched20 10.0.0;}",
            r"{\pict\pngblip\picw100\pich100 89504e470d0a1a0a0000000d49484452}",
            r" the copied phrase}"
        );
        let text = plain_text_from_rtf(word);
        assert_eq!(text, "the copied phrase");
        assert!(!text.contains("89504e47"), "picture hex leaked: {text}");
        assert!(!text.contains("Calibri"), "font table leaked: {text}");
        assert!(!text.contains("Riched20"), "generator leaked: {text}");
    }

    /// `\binN` payload is raw bytes, not RTF. A `}` in those bytes must not
    /// end the `\pict` skip and leak the rest into searchable text.
    #[test]
    fn rtf_bin_payload_is_not_parsed_as_rtf() {
        let mut rtf = String::from(r"{\rtf1{\pict\bin");
        let payload = b"abc}SECRET";
        rtf.push_str(&payload.len().to_string());
        rtf.push_str(std::str::from_utf8(payload).unwrap());
        rtf.push_str("} the copied phrase}");
        let text = plain_text_from_rtf(&rtf);
        assert_eq!(text, "the copied phrase");
        assert!(
            !text.contains("SECRET"),
            "binary after a payload '}}' leaked: {text}"
        );
    }

    /// Word writes non-ASCII as `\uN` plus an ANSI fallback, often `?`.
    #[test]
    fn rtf_unicode_control_word_emits_the_codepoint() {
        assert_eq!(plain_text_from_rtf(r"{\rtf1\u12354?}"), "あ");
    }
}
