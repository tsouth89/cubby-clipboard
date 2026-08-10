//! Performance and resource budgets (SBS-219).
//!
//! A budget is only useful if exceeding it is a decision rather than a
//! surprise, so every budget here carries the reason it exists and how it is
//! policed.
//!
//! Budgets split into two kinds, and conflating them is what makes performance
//! suites get disabled:
//!
//! * [`Enforcement::Enforced`] budgets are deterministic -- bytes on disk,
//!   bytes in memory, counts. They do not depend on how busy the machine is, so
//!   a test can fail the build on them.
//! * [`Enforcement::Reported`] budgets are wall-clock or whole-process
//!   measurements: cold start, idle CPU, paste completion. They depend on the
//!   machine, the disk, and whatever else is running. They are measured and
//!   printed, and never fail a build, because a timing assertion on shared CI
//!   hardware teaches people to ignore red.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Bytes,
    Millis,
    Percent,
}

impl Unit {
    pub fn format(self, value: f64) -> String {
        match self {
            Unit::Bytes => {
                if value >= 1_048_576.0 {
                    format!("{:.1} MiB", value / 1_048_576.0)
                } else if value >= 1024.0 {
                    format!("{:.1} KiB", value / 1024.0)
                } else {
                    format!("{value:.0} B")
                }
            }
            Unit::Millis => format!("{value:.0} ms"),
            Unit::Percent => format!("{value:.1}%"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Enforcement {
    /// Deterministic. A test may fail the build on this.
    Enforced,
    /// Machine-dependent. Measured and reported, never asserted.
    Reported,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Budget {
    pub id: &'static str,
    /// What is measured, in the terms a reader would use.
    pub what: &'static str,
    pub limit: f64,
    pub unit: Unit,
    pub enforcement: Enforcement,
    /// Why this number. A budget without a rationale gets raised the first time
    /// it fails.
    pub why: &'static str,
}

/// The budgets named in SBS-219.
///
/// Limits are set from measurements on the development machine with headroom,
/// not from aspiration; `docs/PERFORMANCE_BUDGETS.md` records the observed
/// values each one was derived from.
pub const BUDGETS: &[Budget] = &[
    Budget {
        id: "db_growth_per_text_clip",
        what: "database bytes per stored text clip",
        // Observed 922 B for a 170-byte prose clip. Set at roughly 2x so a
        // real regression trips it while ordinary variation does not; a budget
        // with 4x headroom never catches anything.
        limit: 2048.0,
        unit: Unit::Bytes,
        enforcement: Enforcement::Enforced,
        why: "History is uncapped by default, so per-clip cost sets how fast \
              the file grows. Encryption and the content hash dominate; a \
              regression here usually means a new per-clip column or an index \
              that stores the plaintext twice.",
    },
    Budget {
        id: "search_index_bytes_per_clip",
        what: "in-memory search index bytes per indexed clip",
        // Observed 998 B after the postings rework (#169), down from 5.2 KiB
        // when postings were a HashSet<Arc<str>> per trigram. Set at roughly 2x
        // so the old representation could not come back unnoticed.
        limit: 2048.0,
        unit: Unit::Bytes,
        // Reported, not enforced, despite being a byte count: measuring it
        // needs a process-wide allocation counter, which other tests running in
        // parallel would pollute. It is measured by the ignored perf tests,
        // which the script runs single-threaded.
        enforcement: Enforcement::Reported,
        why: "The trigram index is held in memory for the life of the process \
              because the database is encrypted and cannot be searched by SQL. \
              It is the single largest resident structure, so its per-clip cost \
              is what decides idle memory on a large history.",
    },
    Budget {
        id: "image_thumbnail_bytes",
        what: "stored thumbnail size for a captured screenshot",
        // Observed 86.8 KiB for a 1920x1080 gradient, which compresses worse
        // than most real screenshots.
        limit: 196_608.0,
        unit: Unit::Bytes,
        enforcement: Enforcement::Enforced,
        why: "Thumbnails are retained after full image blobs are pruned, so \
              they outlive the clip they came from and accumulate.",
    },
    Budget {
        id: "first_searchable_result",
        what: "search query latency over a large history",
        limit: 100.0,
        unit: Unit::Millis,
        enforcement: Enforcement::Reported,
        why: "Search runs on every keystroke in the flyout. Past 100 ms the \
              list visibly lags typing.",
    },
    Budget {
        id: "process_startup",
        // Not "to a visible window". Cubby starts to the tray and creates its
        // flyout hidden, so there is no visible window to wait for at launch --
        // measuring one would report "never". Time from the hotkey to something
        // on screen is a separate budget, `shortcut_to_visible`.
        what: "process start to the main window existing (created hidden)",
        limit: 2000.0,
        unit: Unit::Millis,
        enforcement: Enforcement::Reported,
        why: "Cubby starts with the session; a slow start is invisible to the \
              user but delays the first hotkey after login.",
    },
    Budget {
        id: "shortcut_to_visible",
        what: "hotkey press to flyout visible",
        limit: 150.0,
        unit: Unit::Millis,
        enforcement: Enforcement::Reported,
        why: "This is the interaction the product is judged on. Beyond roughly \
              150 ms the flyout feels summoned rather than already there.",
    },
    Budget {
        id: "paste_completion",
        what: "paste request to text delivered to the target",
        limit: 800.0,
        unit: Unit::Millis,
        enforcement: Enforcement::Reported,
        why: "Bounded by the deliberate settle delays in the paste engine \
              (100 ms standard, 600 ms remote session), so this budget tracks \
              those rather than constraining them.",
    },
    Budget {
        id: "idle_cpu",
        what: "CPU use while idle with a populated history",
        limit: 1.0,
        unit: Unit::Percent,
        enforcement: Enforcement::Reported,
        why: "Cubby runs all day. Idle cost is the difference between a utility \
              and something users uninstall to save battery.",
    },
    Budget {
        id: "idle_memory",
        what: "working set while idle with a populated history",
        limit: 209_715_200.0,
        unit: Unit::Bytes,
        enforcement: Enforcement::Reported,
        why: "Dominated by the in-memory search index and the WebView. Tracked \
              as a whole-process number because that is what Task Manager \
              shows and what users report.",
    },
];

pub fn budget(id: &str) -> Option<&'static Budget> {
    BUDGETS.iter().find(|budget| budget.id == id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Within an enforced budget.
    Within,
    /// Over an enforced budget. This is a failure.
    Exceeded,
    /// A reported budget, measured but not asserted. Carries whether the value
    /// was over the limit so a report can say so without failing.
    Reported { over: bool },
}

impl Verdict {
    pub fn is_failure(self) -> bool {
        matches!(self, Verdict::Exceeded)
    }
}

pub fn evaluate(budget: &Budget, value: f64) -> Verdict {
    let over = value > budget.limit;
    match budget.enforcement {
        Enforcement::Enforced if over => Verdict::Exceeded,
        Enforcement::Enforced => Verdict::Within,
        Enforcement::Reported => Verdict::Reported { over },
    }
}

#[derive(Debug, Clone)]
pub struct Measurement {
    pub budget_id: &'static str,
    pub value: f64,
}

#[derive(Debug, Clone)]
pub struct MeasuredBudget {
    pub budget: &'static Budget,
    pub value: f64,
    pub verdict: Verdict,
}

impl fmt::Display for MeasuredBudget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let status = match self.verdict {
            Verdict::Within => "ok".to_string(),
            Verdict::Exceeded => "OVER".to_string(),
            Verdict::Reported { over } => {
                if over {
                    "over (reported)".to_string()
                } else {
                    "ok (reported)".to_string()
                }
            }
        };
        write!(
            formatter,
            "{:<28} {:>12} / {:<12} {}",
            self.budget.id,
            self.budget.unit.format(self.value),
            self.budget.unit.format(self.budget.limit),
            status
        )
    }
}

/// Pair measurements with their budgets. Unknown ids are an error rather than
/// being dropped: a typo would otherwise silently remove a budget from the
/// report while everything still looked green.
pub fn measure(measurements: &[Measurement]) -> Result<Vec<MeasuredBudget>, String> {
    measurements
        .iter()
        .map(|measurement| {
            let budget = budget(measurement.budget_id)
                .ok_or_else(|| format!("unknown budget id '{}'", measurement.budget_id))?;
            Ok(MeasuredBudget {
                budget,
                value: measurement.value,
                verdict: evaluate(budget, measurement.value),
            })
        })
        .collect()
}

/// Budgets with no measurement in this run. Reported so a partial run is never
/// mistaken for a clean one.
pub fn unmeasured(measurements: &[Measurement]) -> Vec<&'static Budget> {
    BUDGETS
        .iter()
        .filter(|budget| {
            !measurements
                .iter()
                .any(|measurement| measurement.budget_id == budget.id)
        })
        .collect()
}

/// Measurements against the budgets above.
///
/// The enforced ones assert, because they are byte counts that do not vary with
/// machine load. The reported ones are `#[ignore]`d and print their numbers:
/// they need either a quiet machine or a process-wide allocation counter, and
/// `scripts/measure-perf-budgets.ps1` runs them single-threaded.
/// Counts live heap bytes so the search index's resident cost can be measured.
///
/// There is no way to ask a Rust structure how much heap it owns, and the
/// trigram index is a graph of `HashMap`s and `String`s whose size cannot be
/// derived from its field types. Wrapping the allocator is the only honest
/// measurement. It is `cfg(test)` only, so nothing ships with it, and the
/// measurement that uses it is `#[ignore]`d because a parallel test run would
/// have other threads allocating into the same counter.
#[cfg(test)]
mod counting_allocator {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicIsize, Ordering};

    pub static LIVE_BYTES: AtomicIsize = AtomicIsize::new(0);

    pub struct Counting;

    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let pointer = unsafe { System.alloc(layout) };
            if !pointer.is_null() {
                LIVE_BYTES.fetch_add(layout.size() as isize, Ordering::Relaxed);
            }
            pointer
        }

        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            LIVE_BYTES.fetch_sub(layout.size() as isize, Ordering::Relaxed);
            unsafe { System.dealloc(pointer, layout) }
        }

        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            let new_pointer = unsafe { System.realloc(pointer, layout, new_size) };
            if !new_pointer.is_null() {
                LIVE_BYTES.fetch_add(
                    new_size as isize - layout.size() as isize,
                    Ordering::Relaxed,
                );
            }
            new_pointer
        }
    }

    pub fn live_bytes() -> isize {
        LIVE_BYTES.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
#[global_allocator]
static COUNTING_ALLOCATOR: counting_allocator::Counting = counting_allocator::Counting;

#[cfg(test)]
mod measurements {
    use super::*;
    use crate::database::Database;
    use uuid::Uuid;

    /// Representative clip: a paragraph of prose, which is what most captured
    /// text is. Sizing the budget off a one-word clip would make per-clip
    /// overhead look far worse than it is in practice.
    const SAMPLE_TEXT: &str = "The quick brown fox jumps over the lazy dog near the riverbank, \
         and then pauses to consider whether the crossing is worth the effort at this hour.";

    const SAMPLE_CLIPS: usize = 200;

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("cubby-perf-{label}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    async fn on_disk_database(dir: &std::path::Path) -> (Database, std::path::PathBuf) {
        let path = dir.join("cubby.db");
        let database = Database::new(path.to_str().expect("utf-8 path"))
            .await
            .expect("database should open");
        database.migrate().await.expect("migration should succeed");
        (database, path)
    }

    async fn insert_text_clip(database: &Database, text: &str) {
        let material = crate::clipboard::build_clip_hash_material(
            "text",
            text.as_bytes(),
            std::iter::empty::<(&str, &[u8])>(),
        );
        sqlx::query(
            r#"INSERT INTO clips (uuid, clip_type, content, text_preview, content_hash)
               VALUES (?, 'text', ?, ?, ?)"#,
        )
        .bind(Uuid::new_v4().to_string())
        .bind(database.crypto.encrypt(text.as_bytes()).unwrap())
        .bind(database.crypto.encrypt_text(text).unwrap())
        .bind(database.crypto.keyed_hash(&material))
        .execute(&database.pool)
        .await
        .expect("insert should succeed");
    }

    /// Total bytes of the database and its write-ahead sidecars. Ignoring the
    /// `-wal` file would report a number the user never sees on disk.
    fn database_bytes(path: &std::path::Path) -> u64 {
        let mut total = 0;
        for suffix in ["", "-wal", "-shm"] {
            let candidate = std::path::PathBuf::from(format!("{}{suffix}", path.display()));
            if let Ok(metadata) = std::fs::metadata(&candidate) {
                total += metadata.len();
            }
        }
        total
    }

    fn sample_screenshot_png() -> Vec<u8> {
        // A gradient rather than a flat fill: a solid colour compresses to
        // almost nothing and would make the thumbnail budget meaningless.
        let mut buffer = image::RgbaImage::new(1920, 1080);
        for (x, y, pixel) in buffer.enumerate_pixels_mut() {
            *pixel = image::Rgba([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8, 255]);
        }
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(buffer)
            .write_to(&mut bytes, image::ImageOutputFormat::Png)
            .expect("sample screenshot should encode");
        bytes.into_inner()
    }

    #[test]
    fn database_growth_per_text_clip_is_within_budget() {
        let dir = temp_dir("db-growth");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        let per_clip = runtime.block_on(async {
            let (database, path) = on_disk_database(&dir).await;
            let empty = database_bytes(&path);
            for _ in 0..SAMPLE_CLIPS {
                insert_text_clip(&database, SAMPLE_TEXT).await;
            }
            // Checkpoint so the measurement reflects the settled file rather
            // than whatever happens to still be sitting in the WAL.
            sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
                .execute(&database.pool)
                .await
                .ok();
            let full = database_bytes(&path);
            (full.saturating_sub(empty)) as f64 / SAMPLE_CLIPS as f64
        });

        let _ = std::fs::remove_dir_all(&dir);

        let budget = budget("db_growth_per_text_clip").expect("budget");
        let measured = MeasuredBudget {
            budget,
            value: per_clip,
            verdict: evaluate(budget, per_clip),
        };
        println!("{measured}");
        assert!(
            !measured.verdict.is_failure(),
            "database grew {} per clip, budget is {} (clip text was {} bytes)",
            Unit::Bytes.format(per_clip),
            Unit::Bytes.format(budget.limit),
            SAMPLE_TEXT.len()
        );
    }

    #[test]
    fn image_thumbnail_stays_within_budget() {
        let png = sample_screenshot_png();
        let thumbnail = crate::clipboard::create_image_preview(&png).expect("preview");
        let value = thumbnail.len() as f64;

        let budget = budget("image_thumbnail_bytes").expect("budget");
        let measured = MeasuredBudget {
            budget,
            value,
            verdict: evaluate(budget, value),
        };
        println!("{measured}");
        assert!(
            !measured.verdict.is_failure(),
            "thumbnail of a 1920x1080 screenshot was {}, budget is {}",
            Unit::Bytes.format(value),
            Unit::Bytes.format(budget.limit)
        );
    }

    /// Reported: needs the process-wide allocation counter, so it is only
    /// meaningful when nothing else is allocating in parallel.
    #[test]
    #[ignore = "allocation measurement; run single-threaded via scripts/measure-perf-budgets.ps1"]
    fn report_search_index_memory_per_clip() {
        const INDEXED_CLIPS: usize = 2_000;

        let dir = temp_dir("index-memory");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        let per_clip = runtime.block_on(async {
            let (database, _) = on_disk_database(&dir).await;
            for index in 0..INDEXED_CLIPS {
                insert_text_clip(&database, &format!("{SAMPLE_TEXT} entry {index}")).await;
            }

            // Measure across the index build only. Everything the rows needed
            // to get into the database is already allocated and freed by now.
            let before = super::counting_allocator::live_bytes();
            database
                .search_index
                .ensure_ready(&database.pool, &database.crypto)
                .await
                .expect("index should build");
            let after = super::counting_allocator::live_bytes();

            // Keep the database alive to here so the index is not dropped
            // before the second reading.
            assert!(!database.search_index.matches("riverbank").is_empty());
            (after - before) as f64 / INDEXED_CLIPS as f64
        });

        let _ = std::fs::remove_dir_all(&dir);

        let budget = budget("search_index_bytes_per_clip").expect("budget");
        println!(
            "{}",
            MeasuredBudget {
                budget,
                value: per_clip,
                verdict: evaluate(budget, per_clip),
            }
        );
    }

    /// Reported, not asserted: query latency depends on the machine.
    #[test]
    #[ignore = "timing measurement; run via scripts/measure-perf-budgets.ps1"]
    fn report_search_latency_over_a_large_history() {
        let dir = temp_dir("search-latency");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        let elapsed = runtime.block_on(async {
            let (database, _) = on_disk_database(&dir).await;
            for index in 0..2_000 {
                insert_text_clip(&database, &format!("{SAMPLE_TEXT} entry {index}")).await;
            }
            database
                .search_index
                .ensure_ready(&database.pool, &database.crypto)
                .await
                .expect("index should build");

            let start = std::time::Instant::now();
            let hits = database.search_index.matches("riverbank");
            let elapsed = start.elapsed();
            assert!(!hits.is_empty(), "the query should match the sample clips");
            elapsed
        });

        let _ = std::fs::remove_dir_all(&dir);

        let budget = budget("first_searchable_result").expect("budget");
        let value = elapsed.as_secs_f64() * 1000.0;
        println!(
            "{}",
            MeasuredBudget {
                budget,
                value,
                verdict: evaluate(budget, value),
            }
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_budget_is_documented_and_unique() {
        let mut seen = Vec::new();
        for budget in BUDGETS {
            assert!(
                !seen.contains(&budget.id),
                "duplicate budget id {}",
                budget.id
            );
            seen.push(budget.id);
            assert!(budget.limit > 0.0, "{} has no limit", budget.id);
            assert!(
                budget.why.len() > 40,
                "{} needs a rationale explaining the number",
                budget.id
            );
            assert!(!budget.what.is_empty());
        }
    }

    #[test]
    fn every_budget_named_by_the_issue_exists() {
        // SBS-219 names these explicitly; dropping one silently would quietly
        // narrow the scope of the work.
        for id in [
            "process_startup",
            "shortcut_to_visible",
            "first_searchable_result",
            "paste_completion",
            "idle_cpu",
            "idle_memory",
            "db_growth_per_text_clip",
            "image_thumbnail_bytes",
        ] {
            assert!(budget(id).is_some(), "missing budget {id}");
        }
    }

    #[test]
    fn enforced_budgets_fail_when_exceeded() {
        let enforced = budget("db_growth_per_text_clip").unwrap();
        assert_eq!(evaluate(enforced, enforced.limit - 1.0), Verdict::Within);
        assert_eq!(evaluate(enforced, enforced.limit), Verdict::Within);
        assert_eq!(evaluate(enforced, enforced.limit + 1.0), Verdict::Exceeded);
        assert!(evaluate(enforced, enforced.limit + 1.0).is_failure());
    }

    #[test]
    fn reported_budgets_never_fail_even_when_over() {
        let reported = budget("idle_cpu").unwrap();
        assert_eq!(
            evaluate(reported, reported.limit * 100.0),
            Verdict::Reported { over: true }
        );
        assert!(!evaluate(reported, reported.limit * 100.0).is_failure());
        assert_eq!(evaluate(reported, 0.0), Verdict::Reported { over: false });
    }

    #[test]
    fn timing_budgets_are_never_enforced() {
        // The whole point of the split: a wall-clock budget must not be able to
        // fail a build on a busy machine.
        for budget in BUDGETS {
            if budget.unit == Unit::Millis || budget.unit == Unit::Percent {
                assert_eq!(
                    budget.enforcement,
                    Enforcement::Reported,
                    "{} is timing-dependent and must not be enforced",
                    budget.id
                );
            }
        }
    }

    #[test]
    fn unknown_measurement_ids_are_rejected() {
        let error = measure(&[Measurement {
            budget_id: "not_a_budget",
            value: 1.0,
        }])
        .unwrap_err();
        assert!(error.contains("not_a_budget"), "{error}");
    }

    #[test]
    fn unmeasured_budgets_are_reported() {
        let measurements = [Measurement {
            budget_id: "idle_cpu",
            value: 0.2,
        }];
        let missing = unmeasured(&measurements);
        assert!(missing.iter().all(|budget| budget.id != "idle_cpu"));
        assert_eq!(missing.len(), BUDGETS.len() - 1);
    }

    #[test]
    fn byte_values_format_readably() {
        assert_eq!(Unit::Bytes.format(512.0), "512 B");
        assert_eq!(Unit::Bytes.format(2048.0), "2.0 KiB");
        assert_eq!(Unit::Bytes.format(3_145_728.0), "3.0 MiB");
        assert_eq!(Unit::Millis.format(97.4), "97 ms");
        // Deliberately not a .x5 value: those land on the float's rounding
        // behaviour rather than on anything this code decides.
        assert_eq!(Unit::Percent.format(0.84), "0.8%");
        assert_eq!(Unit::Percent.format(0.96), "1.0%");
    }
}
