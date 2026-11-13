// The page's timer, in one place.
//
// Everything that made the phone spin lived here: a timer that re-armed itself
// from three callbacks, a flag that could stick, and no pause after a failure.
// This loop has exactly one timer, one guard, and one delay that grows when the
// computer is unreachable.
//
// The scheduler is injected, so the tests run without waiting.

import { createBackoff } from './backoff.mjs';

export function createLoop({
  tick,
  backoff = createBackoff(),
  schedule = (fn, delay) => setTimeout(fn, delay),
  cancel = (handle) => clearTimeout(handle),
  onError = () => {},
}) {
  let handle = null;
  let running = false;
  let stopped = true;
  let again = false;

  async function run() {
    if (running) return;
    running = true;
    try {
      const result = await tick();
      if (result && result.failed) {
        scheduleNext(backoff.failure());
      } else {
        scheduleNext(backoff.success());
      }
    } catch (error) {
      onError(error);
      scheduleNext(backoff.failure());
    } finally {
      running = false;
      if (again) {
        // Somebody asked for a run while this one was busy: do it now.
        again = false;
        run().catch(onError);
      }
    }
  }

  function scheduleNext(delay) {
    if (stopped) return;
    if (handle !== null) cancel(handle);
    handle = schedule(() => {
      run().catch(onError);
    }, delay);
  }

  return {
    /** Begin: the first attempt happens at once, not after a delay. */
    start() {
      if (!stopped) return;
      stopped = false;
      run().catch(onError);
    },
    stop() {
      stopped = true;
      if (handle !== null) cancel(handle);
      handle = null;
    },
    /** Do it now: after stopping a recording, waiting a second feels broken. */
    kick() {
      if (stopped) return;
      if (running) {
        // Never dropped: the running pass will start another one when it ends.
        again = true;
        return;
      }
      if (handle !== null) cancel(handle);
      handle = null;
      backoff.reset();
      run().catch(onError);
    },
    /** For the page: is anything scheduled? */
    get scheduled() {
      return handle !== null;
    },
    get running() {
      return running;
    },
    backoff,
  };
}
