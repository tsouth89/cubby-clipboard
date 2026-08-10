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
| `search_index_bytes_per_clip` | 998 B | 2 KiB | reported |
| `first_searchable_result` | 2–5 ms over 2,000 clips | 100 ms | reported |
| `process_startup` | 254–593 ms | 2,000 ms | reported |
| `idle_cpu` | 0.00–0.16% | 1% | reported |
| `idle_memory` | **424–448 MiB — over** | 200 MiB | reported |
| `shortcut_to_visible` | not measured | 150 ms | reported |
| `paste_completion` | not measured | 800 ms | reported |

The bottom two need a running app driven by a hotkey. The other three were
measured against an installed v1.3.1 with a real 3,735-clip history;
`shortcut_to_visible` and `paste_completion` are not measured by this script and
say so in its output rather than being quietly absent.

**`idle_memory` is over budget: 424–448 MiB against a 200 MiB limit.** Eight or
nine processes, most of them `msedgewebview2` — the WebView2 runtime's renderer,
GPU, utility and crashpad processes. The search index is about 4 MiB of that
after #169, so it is a rounding error here rather than the deciding factor.

An earlier version of this document said idle memory would be "met or missed by
the search index rather than by the WebView". That was wrong, and it was written
before anything had been measured. The WebView tree dominates by two orders of
magnitude.

The 200 MiB limit was set from aspiration, not observation. It has deliberately
**not** been raised to match reality: re-baselining a budget to whatever the code
currently does turns it into a description instead of a constraint. Either the
number is wrong for a WebView2 application, or the application is heavier than
intended — that is a decision to make explicitly, not to paper over. Being a
`Reported` budget, it fails nothing in the meantime.

Two ways this measurement got the wrong answer first, both now fixed and worth
not reintroducing:

- **Scope the process tree by parent id, not by name.** Matching every
  `*WebView*` process on the machine pulled in 48 unrelated ones from other
  apps and reported 1,055 MiB against a 200 MiB budget.
- **Let the app settle before sampling.** Sampling at launch measures the index
  build over the whole history and reported 3.67% CPU where the settled figure
  is 0.00%.

Measuring against a locally built `cubby.exe` requires no other Cubby instance
to be running: the single-instance plugin hands off to the existing process and
exits, which would make the startup number meaningless.

## What the numbers say

**Search index memory was the scaling constraint, and is now much less of
one.** It began at 5.2 KiB per clip — roughly 500 MiB at 100,000 clips, against
a 200 MiB `idle_memory` budget — because each trigram held a `HashSet` of
reference-counted clip ids. Issue #169 replaced that with sorted `u32` slots and
stopped storing a preview the content already contains, bringing it to 998 B per
clip: about 95 MiB at 100,000 clips, and search got faster rather than slower.

The index is still held in memory for the life of the process, because the
database is encrypted and cannot be searched by SQL (SBS-211), so it remains
the largest resident structure and the thing `idle_memory` will be decided by.
It now leaves room for a large history rather than consuming the whole budget
before the WebView is counted.

**Per-clip database cost is not the problem.** 922 B for a 170-byte clip is
mostly encryption overhead and the content hash, and it grows linearly.

## Adding a budget

Add it to `BUDGETS` with a `why` that explains the number, not just the metric —
the unit test rejects a rationale shorter than a sentence, because a budget
without one gets raised the first time it fails instead of investigated. Then
either assert it from a measurement test (if deterministic) or measure and print
it (if not). `unmeasured()` lists budgets that no measurement covered, so a
budget added without a measurement is visible rather than decorative.
