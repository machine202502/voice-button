// Where the pairing token lives, and how it arrives.
//
// The token is a bearer token in `localStorage['vb_token']`, the same key the
// old page and the installer's QR flow use. It may also arrive in the URL
// fragment when the page is opened on the public address and then handed over
// to the local one: fragments never reach the server, so the token cannot end
// up in a request log.
//
// Read once, before anything fetches.

const KEY = 'vb_token';

/**
 * Take a token out of the fragment (`#t=…`) and strip it from the address.
 * Answers the token that was moved, or `''`.
 */
export function takeTokenFromHash(): string {
  if (typeof location === 'undefined' || !location.hash.startsWith('#t=')) return '';
  let moved = '';
  try {
    moved = decodeURIComponent(location.hash.slice(3));
  } catch {
    moved = location.hash.slice(3);
  }
  if (moved) writeToken(moved);
  try {
    history.replaceState(null, '', location.pathname + location.search);
  } catch {
    // A browser that refuses history still has the token in storage.
  }
  return moved;
}

/** The bearer token, or `''` before the phone is paired. */
export function readToken(): string {
  try {
    return localStorage.getItem(KEY) ?? '';
  } catch {
    // Private mode may deny storage: the page then behaves like an unpaired one.
    return '';
  }
}

export function writeToken(token: string): void {
  try {
    localStorage.setItem(KEY, token);
  } catch {
    // Nowhere to keep it; the pairing screen will come back on the next load.
  }
}
