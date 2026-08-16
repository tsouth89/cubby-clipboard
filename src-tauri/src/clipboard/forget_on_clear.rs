//! Forget-on-clear lookup classification (SBS-831).
//!
//! Isolated so the `Err` vs `Ok(None)` decision can be tested without the
//! Windows crate. A missing row is gone; a query error is transient.

/// Result of looking up the clip we just took the forget-on-clear marker for.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ForgetClipLookup<T, E> {
    /// Row exists. Continue pin / already-deleted / DELETE logic.
    Found(T),
    /// No row. The clip is actually gone; leave the retry marker dropped.
    AlreadyGone,
    /// SELECT failed. Restore the marker so a later clear can retry.
    Failed(E),
}

impl<T, E> ForgetClipLookup<T, E> {
    pub(crate) fn from_query(result: Result<Option<T>, E>) -> Self {
        match result {
            Ok(Some(row)) => Self::Found(row),
            Ok(None) => Self::AlreadyGone,
            Err(error) => Self::Failed(error),
        }
    }

    /// Transient query failures must put the in-memory marker back. A genuine
    /// missing row must not, or every later clear would keep hunting a gone clip.
    ///
    /// Only the unit tests below call this directly; `clipboard.rs` matches on
    /// the enum variant itself and restores the marker inline in the `Failed`
    /// arm. Kept `pub(crate)` as the documented, tested source of truth for
    /// that policy rather than deleting it.
    #[allow(dead_code)]
    pub(crate) fn restore_retry_marker(&self) -> bool {
        matches!(self, Self::Failed(_))
    }
}

#[cfg(test)]
mod tests {
    use super::ForgetClipLookup;

    #[test]
    fn a_lookup_error_restores_the_retry_marker() {
        let taken = Some("clip-uuid");
        let result: Result<Option<(i64, i64)>, &str> = Err("database is locked");
        let lookup = ForgetClipLookup::from_query(result);
        assert!(
            matches!(lookup, ForgetClipLookup::Failed("database is locked")),
            "a query error must not collapse to already-gone"
        );
        assert!(lookup.restore_retry_marker());
        assert_eq!(
            lookup.restore_retry_marker().then_some(taken).flatten(),
            Some("clip-uuid")
        );
    }

    #[test]
    fn a_missing_row_does_not_restore_the_retry_marker() {
        let taken = Some("clip-uuid");
        let result: Result<Option<(i64, i64)>, &str> = Ok(None);
        let lookup = ForgetClipLookup::from_query(result);
        assert!(matches!(lookup, ForgetClipLookup::AlreadyGone));
        assert!(!lookup.restore_retry_marker());
        assert_eq!(
            lookup.restore_retry_marker().then_some(taken).flatten(),
            None
        );
    }

    #[test]
    fn a_found_row_continues_without_restoring_yet() {
        let result: Result<Option<(i64, i64)>, &str> = Ok(Some((0, 0)));
        let lookup = ForgetClipLookup::from_query(result);
        assert!(matches!(lookup, ForgetClipLookup::Found((0, 0))));
        assert!(
            !lookup.restore_retry_marker(),
            "pin/deleted/delete logic owns the taken marker after a successful lookup"
        );
    }
}
