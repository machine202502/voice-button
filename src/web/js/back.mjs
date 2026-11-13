// Where the phone's back gesture goes.
//
// The page pushes one history entry per screen it opens: a monolog, the
// archive, a sheet. Android's back gesture and the header arrow both walk that
// history, so back leaves the monolog instead of closing the whole app.
//
// The decision is a pure function: the cases are tested in tests/back-test.mjs
// without a browser.

/**
 * What back closes first.
 *
 * `sheet` — an open sheet on top of everything;
 * `monolog` — the open monolog, back to the list;
 * `archive` — the archive view, back to the open list;
 * `none` — nothing of ours is open: the browser may leave, which on the root
 * list is exactly what a phone should do.
 */
export function backTarget({ sheets = 0, monolog = null, archived = false } = {}) {
  if (sheets > 0) return 'sheet';
  if (monolog) return 'monolog';
  if (archived) return 'archive';
  return 'none';
}

/** The history entry a screen pushes, so `popstate` knows what it left. */
export function viewState({ monolog = null, archived = false } = {}) {
  if (monolog) return { view: 'monolog', id: monolog.id };
  if (archived) return { view: 'archive' };
  return { view: 'list' };
}
