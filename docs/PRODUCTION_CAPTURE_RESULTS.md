# Production capture results

Test date: 2026-07-16

## Architecture

Cubby's production capture path now:

1. blocks on the native Windows clipboard notification stream;
2. snapshots text or image content immediately with bounded contention retries;
3. places immutable snapshots onto an ordered in-process queue;
4. resolves source application metadata after the payload is safe;
5. filters, deduplicates, persists, and notifies the UI from a single consumer.

The inherited 150 ms debounce that discarded all but the newest notification has been removed.

## End-to-end local burst

The real `cubby.exe` process was launched against its SQLite store. A separate process wrote 100 distinct text values at 10 ms intervals.

| Copies | Interval | Persisted rows | Distinct hashes | Result |
| ---: | ---: | ---: | ---: | --- |
| 100 | 10 ms | 100 | 100 | Pass |
| 100 | 5 ms | 100 | 100 | Pass |

Test rows were selected by their dedicated `CUBBY-PROBE-` prefix and removed after verification.

## Previously established listener results

- Native probe: 100/100 text copies at 25 ms.
- Native probe: 100/100 text copies at 10 ms.
- NinjaOne: 20/20 mixed remote items, consisting of 16 text values and 4 screenshots, with zero read failures.

## Current boundary

The production schema now preserves text, HTML, RTF, images, and physical file
lists alongside the primary searchable representation. Virtual files and other
application-specific delayed-rendered formats still require targeted fixtures
before Cubby can claim lossless support for them.
