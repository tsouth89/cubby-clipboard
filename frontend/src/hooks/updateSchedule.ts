export const UPDATE_CHECK_INTERVAL_MS = 30 * 60 * 1000;

/** What the scheduler needs to know about an update; a subset of the plugin's type. */
export interface DiscoveredUpdate {
  available: boolean;
  version: string;
}

export interface UpdateScheduleDeps {
  /** Resolves to the update, or null/unavailable when already current. */
  check: () => Promise<DiscoveredUpdate | null>;
  /** Called once per newly discovered version. */
  announce: (update: DiscoveredUpdate) => void;
  /** Failures are reported here and otherwise swallowed. */
  onError?: (error: unknown) => void;
}

/**
 * The update-check lifecycle, with no React and no Tauri in it.
 *
 * Everything worth regression-testing lives here rather than in the hook: check
 * on start and every {@link UPDATE_CHECK_INTERVAL_MS}, never run two checks at
 * once, announce a given version only once, and go quiet after stop -- including
 * for a check that was already in flight when stop was called, whose result
 * would otherwise arrive and prompt a user who has navigated away.
 *
 * Returns the stop function.
 */
export function startUpdateChecks(deps: UpdateScheduleDeps): () => void {
  const { check, announce, onError } = deps;

  let active = true;
  let inFlight = false;
  // Every version announced so far, not just the last one: if a release is
  // yanked the latest version can go backwards, and remembering only the most
  // recent would re-prompt for a version the user already dismissed.
  const notifiedVersions = new Set<string>();

  const runCheck = async () => {
    // The in-flight guard matters on a slow network: the interval keeps firing
    // and would otherwise stack concurrent checks.
    if (!active || inFlight) return;
    inFlight = true;

    let update: DiscoveredUpdate | null = null;
    try {
      update = await check();
    } catch (error) {
      onError?.(error);
    } finally {
      inFlight = false;
    }

    // Re-read `active`: stop may have been called while the check was awaiting.
    if (!active || !update?.available || notifiedVersions.has(update.version)) return;

    notifiedVersions.add(update.version);
    announce(update);
  };

  void runCheck();
  const intervalId = setInterval(() => void runCheck(), UPDATE_CHECK_INTERVAL_MS);

  return () => {
    active = false;
    clearInterval(intervalId);
  };
}
