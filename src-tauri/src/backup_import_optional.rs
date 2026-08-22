//! Optional backup-import fields: absent, encrypted, or failed (SBS-980).
//!
//! `encrypt_optional_text` already returns `Result<Option<String>, String>` —
//! None is "the bundle had no value", Err is "a value was present and encrypt
//! failed". Import used to collapse those with `.ok().flatten()`, store SQL
//! NULL for notes / source / OCR, and still set `ocr_status = completed` from
//! the bundle field. Failed is unknown, not absent.
//!
//! This file has no crate dependencies so `rustc --test` can pin the contract
//! on a Linux box that cannot compile the Windows crate.

/// Bundle fields that are encrypted only when present.
#[derive(Debug, Clone, Copy)]
pub struct OptionalImportPlaintexts<'a> {
    pub source_app: Option<&'a str>,
    pub source_icon: Option<&'a str>,
    pub metadata: Option<&'a str>,
    pub ocr_text: Option<&'a str>,
    pub notes: Option<&'a str>,
}

/// Ciphertexts to bind on insert. `ocr_status` follows `ocr_text` here, not
/// the bundle field: completed only if we actually stored recognized text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptedImportOptionals {
    pub source_app: Option<String>,
    pub source_icon: Option<String>,
    pub metadata: Option<String>,
    pub ocr_text: Option<String>,
    pub notes: Option<String>,
    pub ocr_status: Option<&'static str>,
}

/// Encrypt every optional import field. The first encrypt error fails the
/// clip — same as content and preview — instead of becoming SQL NULL.
pub fn encrypt_imported_optionals(
    encrypt: impl Fn(Option<&str>) -> Result<Option<String>, String>,
    fields: OptionalImportPlaintexts<'_>,
) -> Result<EncryptedImportOptionals, String> {
    let source_app = require_optional("source_app", encrypt(fields.source_app))?;
    let source_icon = require_optional("source_icon", encrypt(fields.source_icon))?;
    let metadata = require_optional("metadata", encrypt(fields.metadata))?;
    let ocr_text = require_optional("ocr_text", encrypt(fields.ocr_text))?;
    let notes = require_optional("notes", encrypt(fields.notes))?;
    let ocr_status = ocr_status_for_stored_text(ocr_text.as_deref());
    Ok(EncryptedImportOptionals {
        source_app,
        source_icon,
        metadata,
        ocr_text,
        notes,
        ocr_status,
    })
}

fn require_optional(
    field: &'static str,
    result: Result<Option<String>, String>,
) -> Result<Option<String>, String> {
    result.map_err(|error| format!("{field} encrypt failed: {error}"))
}

/// Completed only when the ciphertext we will store is present. A failed
/// encrypt never reaches here; an absent field is not completed.
fn ocr_status_for_stored_text(ocr_text: Option<&str>) -> Option<&'static str> {
    ocr_text.map(|_| "completed")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_encrypt(value: Option<&str>) -> Result<Option<String>, String> {
        Ok(value.map(|text| format!("enc:{text}")))
    }

    fn fail_present(value: Option<&str>) -> Result<Option<String>, String> {
        match value {
            Some(_) => Err("failed to generate nonce: unavailable".to_string()),
            None => Ok(None),
        }
    }

    /// Honest empty: no optional fields means no OCR status.
    #[test]
    fn absent_fields_stay_absent_and_ocr_is_not_completed() {
        let stored = encrypt_imported_optionals(
            ok_encrypt,
            OptionalImportPlaintexts {
                source_app: None,
                source_icon: None,
                metadata: None,
                ocr_text: None,
                notes: None,
            },
        )
        .expect("absent fields are not an encrypt error");
        assert_eq!(stored.notes, None);
        assert_eq!(stored.source_app, None);
        assert_eq!(stored.ocr_text, None);
        assert_eq!(stored.ocr_status, None);
    }

    /// Happy path: stored OCR text is what marks completed, and notes survive.
    #[test]
    fn present_fields_encrypt_and_ocr_completes_from_stored_text() {
        let stored = encrypt_imported_optionals(
            ok_encrypt,
            OptionalImportPlaintexts {
                source_app: Some("Notepad"),
                source_icon: Some("icon"),
                metadata: Some("<html>"),
                ocr_text: Some("recognized words"),
                notes: Some("keep this note"),
            },
        )
        .expect("a successful encrypt must keep every field");
        assert_eq!(stored.notes.as_deref(), Some("enc:keep this note"));
        assert_eq!(stored.source_app.as_deref(), Some("enc:Notepad"));
        assert_eq!(stored.ocr_text.as_deref(), Some("enc:recognized words"));
        assert_eq!(stored.ocr_status, Some("completed"));
    }

    /// SBS-980: encrypt error is unknown, not absent. Flattening it would
    /// return Ok with NULL notes/source/OCR and ocr_status = completed.
    #[test]
    fn optional_encrypt_error_is_not_absent_and_does_not_complete_ocr() {
        let error = encrypt_imported_optionals(
            fail_present,
            OptionalImportPlaintexts {
                source_app: Some("Notepad"),
                source_icon: Some("icon"),
                metadata: Some("<html>"),
                ocr_text: Some("recognized words"),
                notes: Some("keep this note"),
            },
        )
        .expect_err("an encrypt error must fail the clip, not drop fields");
        assert!(
            error.contains("encrypt failed"),
            "the caller should see which field failed: {error}"
        );
        assert!(
            !error.contains("keep this note") && !error.contains("recognized words"),
            "the error must not echo clipboard contents: {error}"
        );
    }

    /// A notes-only encrypt failure still fails the clip. Succeeding with
    /// notes=NULL is the data-loss the ticket names.
    #[test]
    fn notes_encrypt_error_fails_the_clip() {
        let encrypt = |value: Option<&str>| match value {
            Some("keep this note") => Err("failed to generate nonce: unavailable".to_string()),
            other => ok_encrypt(other),
        };
        let error = encrypt_imported_optionals(
            encrypt,
            OptionalImportPlaintexts {
                source_app: Some("Notepad"),
                source_icon: None,
                metadata: None,
                ocr_text: Some("recognized words"),
                notes: Some("keep this note"),
            },
        )
        .expect_err("a notes encrypt error must not store the clip");
        assert!(
            error.starts_with("notes encrypt failed:"),
            "the error should name notes: {error}"
        );
    }

    /// OCR status is derived from stored ciphertext. Bundle-had-text is not
    /// enough: if ocr_text encrypt failed we never return completed.
    #[test]
    fn ocr_status_follows_stored_ciphertext_not_the_bundle_field() {
        assert_eq!(ocr_status_for_stored_text(None), None);
        assert_eq!(
            ocr_status_for_stored_text(Some("enc:words")),
            Some("completed")
        );
    }
}
