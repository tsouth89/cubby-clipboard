import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { startUpdateChecks, UPDATE_CHECK_INTERVAL_MS } from './updateSchedule';

/** A check whose resolution the test controls, so overlap is observable. */
function deferredCheck() {
  const resolvers: ((value: { available: boolean; version: string } | null) => void)[] = [];
  const check = vi.fn(
    () =>
      new Promise<{ available: boolean; version: string } | null>((resolve) => {
        resolvers.push(resolve);
      })
  );
  return { check, resolvers };
}

describe('startUpdateChecks', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  it('checks immediately on start', () => {
    const check = vi.fn().mockResolvedValue(null);
    const stop = startUpdateChecks({ check, announce: vi.fn() });

    expect(check).toHaveBeenCalledTimes(1);
    stop();
  });

  it('checks again after the interval elapses', async () => {
    const check = vi.fn().mockResolvedValue(null);
    const stop = startUpdateChecks({ check, announce: vi.fn() });
    await vi.advanceTimersByTimeAsync(0);

    // One tick short of the interval must not trigger a second check.
    await vi.advanceTimersByTimeAsync(UPDATE_CHECK_INTERVAL_MS - 1);
    expect(check).toHaveBeenCalledTimes(1);

    await vi.advanceTimersByTimeAsync(1);
    expect(check).toHaveBeenCalledTimes(2);

    await vi.advanceTimersByTimeAsync(UPDATE_CHECK_INTERVAL_MS);
    expect(check).toHaveBeenCalledTimes(3);
    stop();
  });

  it('skips a scheduled check while one is still in flight', async () => {
    const { check, resolvers } = deferredCheck();
    const stop = startUpdateChecks({ check, announce: vi.fn() });
    expect(check).toHaveBeenCalledTimes(1);

    // Two intervals pass without the first check ever resolving.
    await vi.advanceTimersByTimeAsync(UPDATE_CHECK_INTERVAL_MS * 2);
    expect(check).toHaveBeenCalledTimes(1);

    // Once it settles, the next interval is free to run.
    resolvers[0](null);
    await vi.advanceTimersByTimeAsync(UPDATE_CHECK_INTERVAL_MS);
    expect(check).toHaveBeenCalledTimes(2);
    stop();
  });

  it('announces a given version only once across many checks', async () => {
    const check = vi.fn().mockResolvedValue({ available: true, version: '1.3.0' });
    const announce = vi.fn();
    const stop = startUpdateChecks({ check, announce });
    await vi.advanceTimersByTimeAsync(0);

    expect(announce).toHaveBeenCalledTimes(1);
    expect(announce).toHaveBeenCalledWith({ available: true, version: '1.3.0' });

    await vi.advanceTimersByTimeAsync(UPDATE_CHECK_INTERVAL_MS * 3);
    expect(check).toHaveBeenCalledTimes(4);
    expect(announce).toHaveBeenCalledTimes(1);
    stop();
  });

  it('announces a genuinely newer version after an earlier one', async () => {
    const check = vi
      .fn()
      .mockResolvedValueOnce({ available: true, version: '1.3.0' })
      .mockResolvedValue({ available: true, version: '1.4.0' });
    const announce = vi.fn();
    const stop = startUpdateChecks({ check, announce });
    await vi.advanceTimersByTimeAsync(0);
    await vi.advanceTimersByTimeAsync(UPDATE_CHECK_INTERVAL_MS);

    expect(announce).toHaveBeenCalledTimes(2);
    expect(announce).toHaveBeenLastCalledWith({ available: true, version: '1.4.0' });
    stop();
  });

  it('does not announce when there is no update', async () => {
    const announce = vi.fn();
    const stop = startUpdateChecks({
      check: vi.fn().mockResolvedValue({ available: false, version: '1.2.6' }),
      announce,
    });
    await vi.advanceTimersByTimeAsync(0);

    expect(announce).not.toHaveBeenCalled();
    stop();
  });

  it('reports a failed check and keeps the schedule running', async () => {
    const failure = new Error('offline');
    const check = vi.fn().mockRejectedValueOnce(failure).mockResolvedValue(null);
    const onError = vi.fn();
    const stop = startUpdateChecks({ check, announce: vi.fn(), onError });
    await vi.advanceTimersByTimeAsync(0);

    expect(onError).toHaveBeenCalledWith(failure);

    // A rejection must clear the in-flight flag, or the schedule wedges.
    await vi.advanceTimersByTimeAsync(UPDATE_CHECK_INTERVAL_MS);
    expect(check).toHaveBeenCalledTimes(2);
    stop();
  });

  it('runs no further checks after stop', async () => {
    const check = vi.fn().mockResolvedValue(null);
    const stop = startUpdateChecks({ check, announce: vi.fn() });
    await vi.advanceTimersByTimeAsync(0);
    expect(check).toHaveBeenCalledTimes(1);

    stop();
    await vi.advanceTimersByTimeAsync(UPDATE_CHECK_INTERVAL_MS * 5);
    expect(check).toHaveBeenCalledTimes(1);
  });

  it('does not announce a check that was already in flight when stop was called', async () => {
    const { check, resolvers } = deferredCheck();
    const announce = vi.fn();
    const stop = startUpdateChecks({ check, announce });

    // Stop while the request is outstanding, then let it land. Without the
    // post-await active check this prompts a user who has already navigated on.
    stop();
    resolvers[0]({ available: true, version: '1.3.0' });
    await vi.advanceTimersByTimeAsync(0);

    expect(announce).not.toHaveBeenCalled();
  });

  it('never reaches for the network itself', () => {
    // The scheduler only calls the injected check, so a test can never hit
    // GitHub even if the real plugin changes underneath it.
    const fetchSpy = vi.spyOn(globalThis, 'fetch');
    const stop = startUpdateChecks({ check: vi.fn().mockResolvedValue(null), announce: vi.fn() });

    expect(fetchSpy).not.toHaveBeenCalled();
    stop();
  });
});
