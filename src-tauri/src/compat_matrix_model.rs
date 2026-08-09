//! Pure model for the clipboard application-compatibility matrix (SBS-408).
//!
//! The runner in `src/bin/compat_matrix.rs` drives real applications and is
//! therefore only meaningful on a live Windows desktop. Everything that decides
//! *what* to run, *which* rows apply, and *how a failure is attributed* lives
//! here so it can be unit tested without a desktop.

use std::fmt;

/// Classes of paste target the matrix has to cover. These are the application
/// families named in the acceptance criteria; a class is not the same thing as
/// a single app, because which concrete app is present varies by machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum AppClass {
    LegacyWin32,
    PackagedApp,
    Explorer,
    Terminal,
    Browser,
    Ide,
    Office,
    RemoteSession,
    Elevated,
}

impl AppClass {
    pub const ALL: [AppClass; 9] = [
        AppClass::LegacyWin32,
        AppClass::PackagedApp,
        AppClass::Explorer,
        AppClass::Terminal,
        AppClass::Browser,
        AppClass::Ide,
        AppClass::Office,
        AppClass::RemoteSession,
        AppClass::Elevated,
    ];

    pub fn slug(self) -> &'static str {
        match self {
            AppClass::LegacyWin32 => "legacy_win32",
            AppClass::PackagedApp => "packaged_app",
            AppClass::Explorer => "explorer",
            AppClass::Terminal => "terminal",
            AppClass::Browser => "browser",
            AppClass::Ide => "ide",
            AppClass::Office => "office",
            AppClass::RemoteSession => "remote_session",
            AppClass::Elevated => "elevated",
        }
    }

    pub fn from_slug(slug: &str) -> Option<AppClass> {
        AppClass::ALL
            .into_iter()
            .find(|class| class.slug().eq_ignore_ascii_case(slug))
    }
}

impl fmt::Display for AppClass {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.slug())
    }
}

/// Clipboard formats the matrix exercises. `FilePayload` is deliberately part
/// of the matrix even though Cubby never stores file payloads: the assertion is
/// that an ignored payload does not wedge later capture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Format {
    UnicodeText,
    Html,
    Rtf,
    Image,
    FilePayload,
}

impl Format {
    pub fn slug(self) -> &'static str {
        match self {
            Format::UnicodeText => "unicode_text",
            Format::Html => "html",
            Format::Rtf => "rtf",
            Format::Image => "image",
            Format::FilePayload => "file_payload",
        }
    }
}

impl fmt::Display for Format {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.slug())
    }
}

/// The pipeline stages a row walks. A failure names the stage it happened in so
/// "Word broke" becomes "Word / html / restore".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Step {
    Capture,
    Restore,
    Paste,
}

impl Step {
    pub const ALL: [Step; 3] = [Step::Capture, Step::Restore, Step::Paste];

    pub fn slug(self) -> &'static str {
        match self {
            Step::Capture => "capture",
            Step::Restore => "restore",
            Step::Paste => "paste",
        }
    }
}

impl fmt::Display for Step {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.slug())
    }
}

/// Why a row could not run. Every variant names something a reader can act on:
/// install the app, run the suite in a remote session, start an elevated
/// target. A row is never silently dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// No candidate application for the class was found on this machine.
    NoAppInstalled { candidates: Vec<String> },
    /// The class only means something under a condition this machine does not
    /// meet (a real remote session, an elevated foreground target).
    EnvironmentNotApplicable { detail: String },
    /// The operator excluded the class with `--only`.
    Deselected,
}

impl SkipReason {
    pub fn detail(&self) -> String {
        match self {
            SkipReason::NoAppInstalled { candidates } => {
                format!(
                    "no candidate application installed (looked for: {})",
                    candidates.join(", ")
                )
            }
            SkipReason::EnvironmentNotApplicable { detail } => detail.clone(),
            SkipReason::Deselected => "not selected by --only".to_string(),
        }
    }

    /// Deselected rows are an operator choice, not a coverage gap, so they do
    /// not count against the matrix being complete.
    pub fn is_coverage_gap(&self) -> bool {
        !matches!(self, SkipReason::Deselected)
    }
}

/// A single attributable failure: which app, which format, which step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub class: AppClass,
    pub app: String,
    pub format: Format,
    pub step: Step,
    pub detail: String,
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} / {} / {} / {}: {}",
            self.class, self.app, self.format, self.step, self.detail
        )
    }
}

/// What happened to one row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowOutcome {
    Passed { checks: usize },
    Failed { failures: Vec<Failure> },
    Skipped { reason: SkipReason },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowResult {
    pub class: AppClass,
    pub app: String,
    pub formats: Vec<Format>,
    pub outcome: RowOutcome,
}

impl RowResult {
    pub fn passed(&self) -> bool {
        matches!(self.outcome, RowOutcome::Passed { .. })
    }

    pub fn failures(&self) -> &[Failure] {
        match &self.outcome {
            RowOutcome::Failed { failures } => failures,
            _ => &[],
        }
    }
}

/// Aggregate verdict for a whole run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatrixSummary {
    pub passed: bool,
    pub rows_total: usize,
    pub rows_passed: usize,
    pub rows_failed: usize,
    pub rows_skipped: usize,
    pub checks: usize,
    pub failures: Vec<Failure>,
    /// Classes with no row that actually executed. These are the honest
    /// coverage gaps: the matrix ran, but this class proved nothing.
    pub classes_not_covered: Vec<AppClass>,
}

/// Fold row results into a verdict.
///
/// A run passes when nothing failed. Skipped rows do not fail the run -- a
/// machine without Office cannot prove anything about Word -- but they are
/// reported as uncovered classes so a green run is never mistaken for full
/// coverage.
pub fn summarize(results: &[RowResult]) -> MatrixSummary {
    let mut failures = Vec::new();
    let mut rows_passed = 0;
    let mut rows_failed = 0;
    let mut rows_skipped = 0;
    let mut checks = 0;

    for result in results {
        match &result.outcome {
            RowOutcome::Passed { checks: count } => {
                rows_passed += 1;
                checks += count;
            }
            RowOutcome::Failed {
                failures: row_failures,
            } => {
                rows_failed += 1;
                failures.extend(row_failures.iter().cloned());
            }
            RowOutcome::Skipped { .. } => rows_skipped += 1,
        }
    }

    let mut classes_not_covered: Vec<AppClass> = AppClass::ALL
        .into_iter()
        .filter(|class| {
            !results.iter().any(|result| {
                result.class == *class && !matches!(result.outcome, RowOutcome::Skipped { .. })
            })
        })
        .collect();
    classes_not_covered.sort();

    MatrixSummary {
        passed: failures.is_empty(),
        rows_total: results.len(),
        rows_passed,
        rows_failed,
        rows_skipped,
        checks,
        failures,
        classes_not_covered,
    }
}

/// Parse `--only a,b,c` into a class filter. An empty selection means "all".
pub fn parse_class_filter(raw: &str) -> Result<Vec<AppClass>, String> {
    let mut selected = Vec::new();
    for token in raw.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        let class = AppClass::from_slug(token).ok_or_else(|| {
            format!(
                "unknown app class '{token}' (expected one of: {})",
                AppClass::ALL
                    .into_iter()
                    .map(AppClass::slug)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
        if !selected.contains(&class) {
            selected.push(class);
        }
    }
    Ok(selected)
}

/// Whether a class runs given the operator's `--only` selection.
pub fn class_selected(class: AppClass, filter: &[AppClass]) -> bool {
    filter.is_empty() || filter.contains(&class)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(class: AppClass, app: &str, format: Format, step: Step) -> Failure {
        Failure {
            class,
            app: app.to_string(),
            format,
            step,
            detail: "detail".to_string(),
        }
    }

    fn passed_row(class: AppClass, app: &str, checks: usize) -> RowResult {
        RowResult {
            class,
            app: app.to_string(),
            formats: vec![Format::UnicodeText],
            outcome: RowOutcome::Passed { checks },
        }
    }

    fn skipped_row(class: AppClass, reason: SkipReason) -> RowResult {
        RowResult {
            class,
            app: "n/a".to_string(),
            formats: Vec::new(),
            outcome: RowOutcome::Skipped { reason },
        }
    }

    #[test]
    fn every_class_has_a_unique_round_trippable_slug() {
        let mut seen = Vec::new();
        for class in AppClass::ALL {
            assert!(
                !seen.contains(&class.slug()),
                "duplicate slug {}",
                class.slug()
            );
            seen.push(class.slug());
            assert_eq!(AppClass::from_slug(class.slug()), Some(class));
        }
        assert_eq!(
            AppClass::from_slug("LEGACY_WIN32"),
            Some(AppClass::LegacyWin32)
        );
        assert_eq!(AppClass::from_slug("nonsense"), None);
    }

    #[test]
    fn a_run_with_no_failures_passes() {
        let results: Vec<RowResult> = AppClass::ALL
            .into_iter()
            .map(|class| passed_row(class, "app", 2))
            .collect();

        let summary = summarize(&results);
        assert!(summary.passed);
        assert_eq!(summary.rows_passed, 9);
        assert_eq!(summary.checks, 18);
        assert!(summary.classes_not_covered.is_empty());
    }

    #[test]
    fn failures_are_collected_with_full_attribution() {
        let results = vec![
            passed_row(AppClass::LegacyWin32, "edit", 1),
            RowResult {
                class: AppClass::Office,
                app: "winword.exe".to_string(),
                formats: vec![Format::Html, Format::Rtf],
                outcome: RowOutcome::Failed {
                    failures: vec![
                        failure(AppClass::Office, "winword.exe", Format::Html, Step::Restore),
                        failure(AppClass::Office, "winword.exe", Format::Rtf, Step::Paste),
                    ],
                },
            },
        ];

        let summary = summarize(&results);
        assert!(!summary.passed);
        assert_eq!(summary.rows_failed, 1);
        assert_eq!(summary.failures.len(), 2);
        assert_eq!(
            summary.failures[0].to_string(),
            "office / winword.exe / html / restore: detail"
        );
    }

    #[test]
    fn skipped_rows_do_not_fail_the_run_but_are_reported_as_uncovered() {
        let results = vec![
            passed_row(AppClass::LegacyWin32, "edit", 1),
            skipped_row(
                AppClass::Office,
                SkipReason::NoAppInstalled {
                    candidates: vec!["winword.exe".to_string()],
                },
            ),
        ];

        let summary = summarize(&results);
        assert!(summary.passed, "a missing app must not fail the run");
        assert_eq!(summary.rows_skipped, 1);
        assert!(summary.classes_not_covered.contains(&AppClass::Office));
        assert!(!summary.classes_not_covered.contains(&AppClass::LegacyWin32));
    }

    #[test]
    fn a_class_counts_as_covered_only_when_a_row_actually_ran() {
        // Same class skipped once and run once: the executed row wins.
        let results = vec![
            skipped_row(AppClass::Browser, SkipReason::Deselected),
            passed_row(AppClass::Browser, "msedge.exe", 3),
        ];
        let summary = summarize(&results);
        assert!(!summary.classes_not_covered.contains(&AppClass::Browser));

        // Only skipped: not covered.
        let results = vec![skipped_row(AppClass::Browser, SkipReason::Deselected)];
        let summary = summarize(&results);
        assert!(summary.classes_not_covered.contains(&AppClass::Browser));
    }

    #[test]
    fn skip_reasons_explain_themselves() {
        let reason = SkipReason::NoAppInstalled {
            candidates: vec!["winword.exe".to_string(), "wordpad.exe".to_string()],
        };
        assert_eq!(
            reason.detail(),
            "no candidate application installed (looked for: winword.exe, wordpad.exe)"
        );
        assert!(reason.is_coverage_gap());

        assert!(!SkipReason::Deselected.is_coverage_gap());
        assert_eq!(SkipReason::Deselected.detail(), "not selected by --only");
    }

    #[test]
    fn class_filter_parses_and_rejects_unknown_names() {
        assert_eq!(
            parse_class_filter("browser, office").unwrap(),
            vec![AppClass::Browser, AppClass::Office]
        );
        assert_eq!(parse_class_filter("browser,browser").unwrap().len(), 1);
        assert_eq!(parse_class_filter("").unwrap(), Vec::new());
        assert_eq!(parse_class_filter("  ,  ").unwrap(), Vec::new());

        let error = parse_class_filter("browser,teapot").unwrap_err();
        assert!(error.contains("unknown app class 'teapot'"), "{error}");
        assert!(
            error.contains("legacy_win32"),
            "error should list valid names: {error}"
        );
    }

    #[test]
    fn an_empty_filter_selects_everything() {
        for class in AppClass::ALL {
            assert!(class_selected(class, &[]));
        }
        assert!(class_selected(AppClass::Browser, &[AppClass::Browser]));
        assert!(!class_selected(AppClass::Office, &[AppClass::Browser]));
    }
}
