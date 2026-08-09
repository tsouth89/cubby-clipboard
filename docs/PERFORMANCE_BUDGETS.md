# Performance and resource budgets

SBS-219. The budgets themselves live in `src-tauri/src/perf_budget.rs`, each one
carrying the reason it exists. This document covers how they are policed and
what was measured to set them.

## Enforced versus reported

Conflating these two is what makes a performance suite get switched off.

**Enforced** budgets are deterministic — bytes on disk, bytes produced by an
encoder. They do not move with machine load, so `cargo test` asserts them and a
regression fails the build.

**Reported** budgets are wall-clock or whole-process numbers: cold start, idle
CPU, search latency, index memory. They depend on the machine, the disk, and
whatever else is running. They are measured and printed and **never** asserted,
because a timing assertion on shared CI hardware only teaches people to ignore
red. `timing_budgets_are_never_enforced` is a unit test, so no wall-clock budget
can be promoted to enforced by accident.

## Running them

```powershell
cargo test --manifest-path src-tauri/Cargo.toml --lib perf_budget   # enforced only
scripts/measure-perf-budgets.ps1                                    # + reported
scripts/measure-perf-budgets.ps1 -AppPath <path-to-cubby.exe>       # + whole-process
```

The reported measurements run single-threaded on purpose: one of them reads a
process-wide allocation counter, and parallel tests would allocate into it.

## Observed values

Measured on the development machine (Windows 11, x64). Limits are set from these
with roughly 2x headroom — enough that ordinary variation does not trip them,
tight enough that a real regression does.

| Budget | Observed | Limit | Enforcement |
| --- | --- | --- | --- |
| `db_growth_per_text_clip` | 922 B | 2 KiB | enforced |
| `image_thumbnail_bytes` | 86.8 KiB | 192 KiB | enforced |
| `search_index_bytes_per_clip` | 5.2 KiB | 8 KiB | reported |
| `first_searchable_result` | 4–10 ms over 2,000 clips | 100 ms | reported |
| `process_startup` | not yet measured | 2,000 ms | reported |
| `shortcut_to_visible` | not yet measured | 150 ms | reported |
| `paste_completion` | not yet measured | 800 ms | reported |
| `idle_cpu` | not yet measured | 1% | reported |
| `idle_memory` | not yet measured | 200 MiB | reported |

The bottom five need a running app. `scripts/measure-perf-budgets.ps1 -AppPath`
measures startup, idle CPU, and idle memory; `shortcut_to_visible` and
`paste_completion` are not measured by this script and say so in its output
rather than being quietly absent.

Measuring against a locally built `cubby.exe` requires no other Cubby instance
to be running: the single-instance plugin hands off to the existing process and
exits, which would make the startup number meaningless.

## What the numbers say

**Search index memory is the scaling constraint.** At 5.2 KiB per clip the
in-memory trigram index costs roughly 10 MiB at 2,000 clips, but around 500 MiB
at 100,000. History is uncapped by default. The index is held in memory because
the database is encrypted and cannot be searched by SQL (SBS-211), so this is a
consequence of the privacy design rather than an oversight — but it means idle
memory on a very large history is dominated by it, and the `idle_memory` budget
will be met or missed by this number rather than by the WebView.

**Per-clip database cost is not the problem.** 922 B for a 170-byte clip is
mostly encryption overhead and the content hash, and it grows linearly.

## Adding a budget

Add it to `BUDGETS` with a `why` that explains the number, not just the metric —
the unit test rejects a rationale shorter than a sentence, because a budget
without one gets raised the first time it fails instead of investigated. Then
either assert it from a measurement test (if deterministic) or measure and print
it (if not). `unmeasured()` lists budgets that no measurement covered, so a
budget added without a measurement is visible rather than decorative.
