import { describe, expect, it } from 'vitest';
import { customRange, presetRange, toUtcStamp, UNBOUNDED } from './dateRange';

/**
 * Boundaries are the user's local midnight, so the expected UTC stamps depend
 * on the machine's zone. Deriving them the same way the caller would keeps
 * these assertions true wherever CI runs, while still pinning the *shape* of
 * each range (which day boundaries, how many of them, half-open).
 */
const localMidnight = (year: number, month: number, day: number) =>
  toUtcStamp(new Date(year, month - 1, day));

// A Wednesday, mid-afternoon: far enough into the day that a "last 7 days"
// implemented as "now minus 168 hours" would visibly differ.
const NOW = new Date(2026, 2, 11, 15, 30, 0);

describe('toUtcStamp', () => {
  it('emits the fixed-width shape created_at is stored in', () => {
    expect(toUtcStamp(new Date(Date.UTC(2026, 2, 11, 15, 30, 45)))).toBe('2026-03-11 15:30:45');
  });

  it('produces stamps that sort chronologically as plain strings', () => {
    const earlier = toUtcStamp(new Date(Date.UTC(2026, 2, 9, 23, 59, 59)));
    const later = toUtcStamp(new Date(Date.UTC(2026, 2, 10, 0, 0, 0)));
    expect(earlier < later).toBe(true);
  });
});

describe('presetRange', () => {
  it('spans today from local midnight to midnight tomorrow', () => {
    expect(presetRange('today', NOW)).toEqual({
      from: localMidnight(2026, 3, 11),
      to: localMidnight(2026, 3, 12),
    });
  });

  it('spans yesterday only', () => {
    expect(presetRange('yesterday', NOW)).toEqual({
      from: localMidnight(2026, 3, 10),
      to: localMidnight(2026, 3, 11),
    });
  });

  it('counts last 7 days as 7 whole days ending with today', () => {
    // Not "now minus 168 hours" — that would exclude this morning's clips.
    expect(presetRange('last7', NOW)).toEqual({
      from: localMidnight(2026, 3, 5),
      to: localMidnight(2026, 3, 12),
    });
  });

  it('counts last 30 days the same way', () => {
    expect(presetRange('last30', NOW)).toEqual({
      from: localMidnight(2026, 2, 10),
      to: localMidnight(2026, 3, 12),
    });
  });

  it('leaves "any time" unbounded', () => {
    expect(presetRange('all', NOW)).toEqual(UNBOUNDED);
  });

  it('holds up across a month boundary', () => {
    const firstOfMonth = new Date(2026, 2, 1, 9, 0, 0);
    expect(presetRange('yesterday', firstOfMonth)).toEqual({
      from: localMidnight(2026, 2, 28),
      to: localMidnight(2026, 3, 1),
    });
  });
});

describe('customRange', () => {
  it('treats both ends as inclusive days', () => {
    // Same day for both means that whole day, so the upper bound advances.
    expect(customRange('2026-03-11', '2026-03-11')).toEqual({
      from: localMidnight(2026, 3, 11),
      to: localMidnight(2026, 3, 12),
    });
  });

  it('supports an open end on either side', () => {
    expect(customRange('2026-03-11', '')).toEqual({
      from: localMidnight(2026, 3, 11),
      to: null,
    });
    expect(customRange('', '2026-03-11')).toEqual({
      from: null,
      to: localMidnight(2026, 3, 12),
    });
  });

  it('is unbounded when neither end is set', () => {
    expect(customRange('', '')).toEqual(UNBOUNDED);
  });
});
