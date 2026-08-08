/**
 * Date-range filtering for the History window (SOU-585).
 *
 * The backend compares against `clips.created_at`, which is stored as
 * `YYYY-MM-DD HH:MM:SS` in **UTC** by every write path (`CURRENT_TIMESTAMP` and
 * the Ditto importer both use that shape). The format is fixed-width, so a
 * plain string comparison is chronological and uses `idx_clips_created` — but
 * only if the bounds we send are in the same shape and the same zone.
 *
 * Ranges are half-open, `[from, to)`. "Today" is midnight today up to midnight
 * tomorrow, so nothing has to reason about the last representable instant of a
 * day, and a clip copied at 23:59:59.999 still counts as today.
 *
 * Boundaries are the user's *local* midnight — someone asking for "yesterday"
 * means their yesterday — converted to UTC for the comparison.
 */

export type DatePreset = 'all' | 'today' | 'yesterday' | 'last7' | 'last30' | 'custom';

export interface DateRange {
  /** Inclusive lower bound, or null for unbounded. */
  from: string | null;
  /** Exclusive upper bound, or null for unbounded. */
  to: string | null;
}

export const UNBOUNDED: DateRange = { from: null, to: null };

export const DATE_PRESET_LABELS: Record<Exclude<DatePreset, 'custom'>, string> = {
  all: 'Any time',
  today: 'Today',
  yesterday: 'Yesterday',
  last7: 'Last 7 days',
  last30: 'Last 30 days',
};

/** `YYYY-MM-DD HH:MM:SS` in UTC, the shape `created_at` is stored in. */
export function toUtcStamp(date: Date): string {
  return date.toISOString().slice(0, 19).replace('T', ' ');
}

/** Local midnight at the start of the day `date` falls in. */
function startOfLocalDay(date: Date): Date {
  return new Date(date.getFullYear(), date.getMonth(), date.getDate());
}

function addDays(date: Date, days: number): Date {
  const shifted = new Date(date);
  shifted.setDate(shifted.getDate() + days);
  return shifted;
}

export function presetRange(preset: Exclude<DatePreset, 'custom'>, now: Date): DateRange {
  const today = startOfLocalDay(now);
  const tomorrow = addDays(today, 1);

  switch (preset) {
    case 'today':
      return { from: toUtcStamp(today), to: toUtcStamp(tomorrow) };
    case 'yesterday':
      return { from: toUtcStamp(addDays(today, -1)), to: toUtcStamp(today) };
    // "Last 7 days" includes today, so it spans 7 day-boundaries back from
    // tomorrow — not 7 days back from right now, which would silently drop this
    // morning's clips out of the window as the day went on.
    case 'last7':
      return { from: toUtcStamp(addDays(today, -6)), to: toUtcStamp(tomorrow) };
    case 'last30':
      return { from: toUtcStamp(addDays(today, -29)), to: toUtcStamp(tomorrow) };
    case 'all':
    default:
      return UNBOUNDED;
  }
}

/**
 * A custom range from two `YYYY-MM-DD` values as typed into date inputs, which
 * are local dates. Both ends are inclusive to the user: picking the same day
 * for both means that whole day.
 */
export function customRange(from: string, to: string): DateRange {
  return {
    from: from ? toUtcStamp(parseLocalDate(from)) : null,
    // Exclusive upper bound, so advance past the end of the chosen day.
    to: to ? toUtcStamp(addDays(parseLocalDate(to), 1)) : null,
  };
}

function parseLocalDate(value: string): Date {
  const [year, month, day] = value.split('-').map(Number);
  return new Date(year, (month ?? 1) - 1, day ?? 1);
}
