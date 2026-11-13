// How long the phone waits before trying again.
//
// A page that retries on every wake-up burns the battery of a phone that is
// simply out of range. The delay grows with each failure up to a ceiling and
// drops back to the floor as soon as something succeeds — so a normal session
// polls once a second, and a session with no connection settles down instead of
// hammering the air.

/** The fastest the page ever tries again. */
export const FLOOR_MS = 1000;

/** The slowest it ever gets: still quick enough to notice the connection back. */
export const CEILING_MS = 15_000;

/**
 * `failures` is how many attempts in a row have failed. Zero means the last
 * attempt worked, and the page goes back to its normal pace.
 */
export function delayFor(failures) {
  if (!Number.isFinite(failures) || failures <= 0) return FLOOR_MS;
  const grown = FLOOR_MS * Math.pow(2, Math.min(failures, 8));
  return Math.min(CEILING_MS, grown);
}

/** Keeps the count, so the page does not do the arithmetic in three places. */
export function createBackoff() {
  let failures = 0;
  return {
    get failures() {
      return failures;
    },
    /** Something worked. */
    success() {
      failures = 0;
      return FLOOR_MS;
    },
    /** Something failed; returns the delay to use now. */
    failure() {
      failures += 1;
      return delayFor(failures);
    },
    /** The delay that would be used right now. */
    current() {
      return delayFor(failures);
    },
    /** The user came back to the app: start fresh, try at once. */
    reset() {
      failures = 0;
      return FLOOR_MS;
    },
  };
}
