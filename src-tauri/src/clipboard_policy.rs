#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilePayloadPolicy {
    Materialize,
    IgnoreFilePayload,
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
    use super::{classify_file_payload, FilePayloadPolicy};

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
}
