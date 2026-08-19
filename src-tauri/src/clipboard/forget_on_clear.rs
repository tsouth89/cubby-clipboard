//! Forget-on-clear lookup classification (SBS-831).
//!
//! Isolated so the `Err` vs `Ok(None)` decision can be tested without the
//! Windows crate. A missing row is gone; a query error is transient. The taken
//! retry marker travels with the variant so `unwrap_or(None)` cannot drop it
//! without the tests noticing.

/// Result of looking up the clip we just took the forget-on-clear marker for.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ForgetClipLookup<T, M, E> {
    /// Row exists. Continue pin / already-deleted / DELETE logic, still holding
    /// the taken marker for later restore if the delete itself fails.
    Found { row: T, taken: M },
    /// No row. The clip is actually gone; the retry marker stays dropped.
    AlreadyGone,
    /// SELECT failed. Restore `taken` so a later clear can retry.
    Failed { error: E, taken: M },
}

impl<T, M, E> ForgetClipLookup<T, M, E> {
    pub(crate) fn from_query(result: Result<Option<T>, E>, taken: M) -> Self {
        match result {
            Ok(Some(row)) => Self::Found { row, taken },
            Ok(None) => Self::AlreadyGone,
            Err(error) => Self::Failed { error, taken },
        }
    }
}

/// Remaining attempts after this one failed. `None` means stop restoring and
/// waiting — a password manager only clears once (SBS-1003).
pub(crate) fn next_forget_attempts(attempts_left: u8) -> Option<u8> {
    attempts_left.checked_sub(1).filter(|left| *left > 0)
}

#[cfg(test)]
mod tests {
    use super::ForgetClipLookup;

    #[test]
    fn a_lookup_error_keeps_the_taken_marker() {
        let result: Result<Option<(i64, i64)>, &str> = Err("database is locked");
        match ForgetClipLookup::from_query(result, "clip-uuid") {
            ForgetClipLookup::Failed {
                error: "database is locked",
                taken,
            } => assert_eq!(taken, "clip-uuid"),
            other => panic!("query error must keep the marker, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_row_drops_the_taken_marker() {
        let result: Result<Option<(i64, i64)>, &str> = Ok(None);
        match ForgetClipLookup::from_query(result, "clip-uuid") {
            ForgetClipLookup::AlreadyGone => {}
            other => panic!("missing row must drop the marker, got {other:?}"),
        }
    }

    #[test]
    fn a_found_row_keeps_the_taken_marker_for_later_restore() {
        let result: Result<Option<(i64, i64)>, &str> = Ok(Some((0, 0)));
        match ForgetClipLookup::from_query(result, "clip-uuid") {
            ForgetClipLookup::Found { row: (0, 0), taken } => assert_eq!(taken, "clip-uuid"),
            other => panic!("found row must keep the marker for the delete path, got {other:?}"),
        }
    }

    #[test]
    fn a_failed_forget_retries_twice_then_stops() {
        assert_eq!(super::next_forget_attempts(3), Some(2));
        assert_eq!(super::next_forget_attempts(2), Some(1));
        assert_eq!(super::next_forget_attempts(1), None);
        assert_eq!(super::next_forget_attempts(0), None);
    }
}
