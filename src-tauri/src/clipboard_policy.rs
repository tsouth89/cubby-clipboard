#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilePayloadPolicy {
    Materialize,
    IgnoreFilePayload,
}

/// Predefined clipboard formats that count as real image bytes, paired with
/// their Win32 identifiers. Production tests availability by identifier; the
/// probe only has a snapshot of format names, so both read this one table
/// instead of keeping their own lists in sync by hand.
pub(crate) const PREDEFINED_IMAGE_FORMATS: [(&str, u32); 3] =
    [("CF_BITMAP", 2), ("CF_DIB", 8), ("CF_DIBV5", 17)];

/// Image formats that have no predefined identifier and must be resolved with
/// `RegisterClipboardFormatW` before they can be queried.
pub(crate) const REGISTERED_IMAGE_FORMATS: [&str; 1] = ["PNG"];

/// Whether a clipboard format name denotes real image bytes, matching what
/// `clipboard_has_image_format` detects by identifier.
// Only the clipboard probe reaches for the name form -- it validates captured
// format snapshots rather than a live clipboard -- but it lives here so both
// halves of the check stay in one file.
#[allow(dead_code)]
pub(crate) fn is_image_format_name(name: &str) -> bool {
    PREDEFINED_IMAGE_FORMATS
        .iter()
        .any(|(format, _)| *format == name)
        || REGISTERED_IMAGE_FORMATS.contains(&name)
}

/// File descriptors and paths are not durable clipboard history. The one
/// exception is a hybrid payload that also advertises real image bytes: common
/// screenshot tools publish both, and Cubby retains the image rather than the
/// external path.
pub(crate) fn classify_file_payload(
    has_file_payload: bool,
    has_image_payload: bool,
) -> FilePayloadPolicy {
    if has_file_payload && !has_image_payload {
        FilePayloadPolicy::IgnoreFilePayload
    } else {
        FilePayloadPolicy::Materialize
    }
}

#[cfg(test)]
mod tests {
    use super::{
        classify_file_payload, is_image_format_name, FilePayloadPolicy, PREDEFINED_IMAGE_FORMATS,
        REGISTERED_IMAGE_FORMATS,
    };

    #[test]
    fn ignores_physical_or_virtual_files_even_with_text_fallbacks() {
        assert_eq!(
            classify_file_payload(true, false),
            FilePayloadPolicy::IgnoreFilePayload
        );
    }

    #[test]
    fn materializes_non_file_payloads_and_file_backed_images() {
        assert_eq!(
            classify_file_payload(false, false),
            FilePayloadPolicy::Materialize
        );
        assert_eq!(
            classify_file_payload(false, true),
            FilePayloadPolicy::Materialize
        );
        assert_eq!(
            classify_file_payload(true, true),
            FilePayloadPolicy::Materialize
        );
    }

    #[test]
    fn recognizes_every_image_format_by_name() {
        for (name, _) in PREDEFINED_IMAGE_FORMATS {
            assert!(is_image_format_name(name), "{name} should be an image");
        }
        for name in REGISTERED_IMAGE_FORMATS {
            assert!(is_image_format_name(name), "{name} should be an image");
        }
        assert!(!is_image_format_name("CF_UNICODETEXT"));
        assert!(!is_image_format_name("FileGroupDescriptorW"));
    }
}
