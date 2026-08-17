//! History-rewrite notices that must be flushed after the logger exists.
//!
//! `Database::new` and `migrate` run before `tauri_plugin_log` is installed.
//! A `log::` call in that window is discarded. These constructors are the
//! only supported way to record a quarantine, restore, or empty-history
//! fallback so `run_app` can push the line into the existing `startup_log`
//! buffer (SBS-929).
//!
//! This file has no crate dependencies so `rustc --test` can pin the
//! messages on a Linux box that cannot compile the Windows crate.

/// Severity of a pre-logger recovery line. Mapped to `log::Level` at the
/// flush site so this module stays standalone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryLevel {
    Error,
    Warn,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryNotice {
    pub level: RecoveryLevel,
    pub message: String,
}

impl RecoveryNotice {
    fn new(level: RecoveryLevel, message: impl Into<String>) -> Self {
        Self {
            level,
            message: message.into(),
        }
    }
}

/// What happened after the unusable history file was moved aside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreOutcome {
    NoBackup,
    BackupUnusable { reason: String },
    Restored { path: String },
    CopyFailed { error: String },
    TaskFailed { error: String },
}

pub fn quarantine(reason: &str) -> RecoveryNotice {
    RecoveryNotice::new(
        RecoveryLevel::Error,
        format!("STORAGE: Clipboard history database is unusable ({reason}); quarantining"),
    )
}

pub fn no_rolling_backup() -> RecoveryNotice {
    RecoveryNotice::new(
        RecoveryLevel::Warn,
        "STORAGE: No rolling backup found; starting with an empty history",
    )
}

pub fn backup_unusable(reason: &str) -> RecoveryNotice {
    RecoveryNotice::new(
        RecoveryLevel::Error,
        format!(
            "STORAGE: Rolling backup failed verification ({reason}); starting with an empty history"
        ),
    )
}

pub fn restored(path: &str) -> RecoveryNotice {
    RecoveryNotice::new(
        RecoveryLevel::Warn,
        format!("STORAGE: Restored clipboard history from rolling backup {path}"),
    )
}

pub fn restore_copy_failed(error: &str) -> RecoveryNotice {
    RecoveryNotice::new(
        RecoveryLevel::Error,
        format!(
            "STORAGE: Could not restore history backup: {error}; starting with an empty history"
        ),
    )
}

pub fn restore_task_failed(error: &str) -> RecoveryNotice {
    RecoveryNotice::new(
        RecoveryLevel::Error,
        format!("STORAGE: Backup restore task failed: {error}; starting with an empty history"),
    )
}

pub fn backup_refresh_failed(error: &str) -> RecoveryNotice {
    RecoveryNotice::new(
        RecoveryLevel::Warn,
        format!("STORAGE: Could not refresh history backup: {error}"),
    )
}

pub fn removed_file_references(count: u64) -> RecoveryNotice {
    RecoveryNotice::new(
        RecoveryLevel::Info,
        format!("STORAGE: Removed {count} legacy file-reference history items"),
    )
}

/// The lines a corrupt-DB fail-open must return. Returning an empty vec
/// here is the pre-fix bug: `log::` inside `Database::new` and nothing
/// for `startup_log` to flush.
pub fn notices_for_corrupt(reason: &str, restore: RestoreOutcome) -> Vec<RecoveryNotice> {
    let mut notices = vec![quarantine(reason)];
    notices.push(match restore {
        RestoreOutcome::NoBackup => no_rolling_backup(),
        RestoreOutcome::BackupUnusable { reason } => backup_unusable(&reason),
        RestoreOutcome::Restored { path } => restored(&path),
        RestoreOutcome::CopyFailed { error } => restore_copy_failed(&error),
        RestoreOutcome::TaskFailed { error } => restore_task_failed(&error),
    });
    notices
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the failure mode this module exists for: a history rewrite
    /// that only `log::`'d would hand `startup_log` an empty vec.
    #[test]
    fn notices_for_corrupt_are_not_empty() {
        let notices = notices_for_corrupt("file is not a database", RestoreOutcome::NoBackup);
        assert!(
            !notices.is_empty(),
            "a corrupt-DB fail-open must return collectable lines, not log:: and discard"
        );
        assert!(
            notices
                .iter()
                .any(|n| n.level == RecoveryLevel::Error && n.message.contains("quarantining")),
            "expected a quarantine line, got {notices:?}"
        );
        assert!(
            notices.iter().any(|n| n.message.contains("empty history")),
            "expected an empty-history fallback line, got {notices:?}"
        );
    }

    #[test]
    fn successful_restore_is_not_silent() {
        let notices = notices_for_corrupt(
            "quick_check: not ok",
            RestoreOutcome::Restored {
                path: "cubby.db.bak".to_string(),
            },
        );
        assert!(
            notices.iter().any(|n| n
                .message
                .contains("Restored clipboard history from rolling backup")),
            "expected a restore line, got {notices:?}"
        );
        assert!(
            !notices.iter().any(|n| n.message.contains("empty history")),
            "a successful restore must not also claim empty history, got {notices:?}"
        );
    }

    #[test]
    fn unusable_backup_is_an_empty_history_fallback() {
        let notices = notices_for_corrupt(
            "file is not a database",
            RestoreOutcome::BackupUnusable {
                reason: "file is not a database".to_string(),
            },
        );
        assert!(
            notices
                .iter()
                .any(|n| n.message.contains("Rolling backup failed verification")
                    && n.message.contains("empty history")),
            "expected an unusable-backup fallback, got {notices:?}"
        );
    }

    #[test]
    fn restore_copy_and_task_failures_are_empty_history_fallbacks() {
        for outcome in [
            RestoreOutcome::CopyFailed {
                error: "disk full".to_string(),
            },
            RestoreOutcome::TaskFailed {
                error: "cancelled".to_string(),
            },
        ] {
            let notices = notices_for_corrupt("quick_check: not ok", outcome);
            assert!(
                notices.iter().any(|n| n.message.contains("empty history")),
                "expected an empty-history fallback, got {notices:?}"
            );
        }
    }

    #[test]
    fn backup_refresh_failure_and_file_reference_removal_keep_their_wording() {
        assert!(backup_refresh_failed("disk full")
            .message
            .contains("Could not refresh history backup"));
        assert_eq!(
            removed_file_references(3).message,
            "STORAGE: Removed 3 legacy file-reference history items"
        );
    }
}
