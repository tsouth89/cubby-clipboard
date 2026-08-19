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

/// Which capture a forget-on-clear attempt acts on.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ForgetAttempt<T> {
    /// A retry deletes the capture it was scheduled for, full stop.
    Pinned(T),
    /// A first attempt takes the marker, but only inside the forget window.
    FromMarker(T),
    /// Nothing to forget.
    Nothing,
}

/// Pick the capture for this attempt.
///
/// A pinned capture wins outright. The first attempt holds CLIPBOARD_SYNC
/// across its lookup, so queued snapshots land the moment it fails and
/// overwrite the marker; a retry that re-read the marker would delete one of
/// those copies instead of the password the clear was about. Re-checking the
/// window is wrong for the same reason: a capture 89.9s old would age past 90s
/// during the retry delay and escape a forget it had already earned.
pub(crate) fn select_forget_attempt<T>(
    pinned: Option<T>,
    marker: Option<T>,
    marker_within_window: bool,
) -> ForgetAttempt<T> {
    if let Some(capture) = pinned {
        return ForgetAttempt::Pinned(capture);
    }
    match marker {
        Some(capture) if marker_within_window => ForgetAttempt::FromMarker(capture),
        _ => ForgetAttempt::Nothing,
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
    #[test]
    fn a_retry_deletes_its_own_capture_not_whatever_landed_since() {
        // SQLITE_BUSY holds the first attempt with CLIPBOARD_SYNC taken, so a
        // queued copy overwrites the marker the moment it fails. The retry must
        // still delete the password, not that copy.
        assert_eq!(
            super::select_forget_attempt(Some("password"), Some("later-copy"), true),
            super::ForgetAttempt::Pinned("password")
        );
    }

    #[test]
    fn a_retry_ignores_the_forget_window() {
        // A capture 89.9s old ages past 90s during the retry delay. It earned
        // the forget on the first attempt and must not lose it to the clock.
        assert_eq!(
            super::select_forget_attempt(Some("password"), None, false),
            super::ForgetAttempt::Pinned("password")
        );
    }

    #[test]
    fn a_first_attempt_takes_the_marker_only_inside_the_window() {
        assert_eq!(
            super::select_forget_attempt(None, Some("recent"), true),
            super::ForgetAttempt::FromMarker("recent")
        );
        assert_eq!(
            super::select_forget_attempt(None, Some("stale"), false),
            super::ForgetAttempt::Nothing
        );
        assert_eq!(
            super::select_forget_attempt::<&str>(None, None, true),
            super::ForgetAttempt::Nothing
        );
    }
}
