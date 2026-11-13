// The shape of the message list on the phone.
//
// The page shows a chat: oldest on top, newest at the bottom, one divider per
// day. The computer stores messages in the order it received them, which is not
// the same thing — a phone that was offline for an hour uploads late. So the
// list is built here, from the phone's own timestamps, and the page only draws
// what it gets.

/**
 * Oldest first, by the phone's own clock. Two takes can share a stamp: without
 * a tie-breaker the redraw could put them in the other order and the rows
 * looked like they were swapping places.
 */
export function sortAscending(messages) {
  const idOf = (message) => {
    const id = Number(message.id ?? message.localId);
    return Number.isFinite(id) ? id : 0;
  };
  return [...messages].sort((left, right) => {
    const a = String(left.recorded || '');
    const b = String(right.recorded || '');
    if (a < b) return -1;
    if (a > b) return 1;
    return idOf(left) - idOf(right);
  });
}

/** `2026-11-12T15:08:41+05:00` → `2026-11-12`. Anything unreadable is `''`. */
export function dayKey(recorded) {
  const value = String(recorded || '');
  const cut = value.indexOf('T');
  return cut > 0 ? value.slice(0, cut) : '';
}

/**
 * Turn messages into a list of rows: `{ kind: 'day', key }` once per day and
 * `{ kind: 'message', message }` for each take.
 *
 * Nothing is dropped: a message with an unreadable timestamp goes to its own
 * monolog at the end rather than disappearing from the phone.
 */
/**
 * Which way a finger is going, once it has gone far enough. A vertical move is
 * a scroll, not a swipe; a short diagonal move is still a tap.
 */
export function swipeDirection(dx, dy, threshold = 72) {
  if (Math.abs(dx) < threshold || Math.abs(dx) <= Math.abs(dy)) return null;
  return dx < 0 ? 'left' : 'right';
}

/** The message the microphone is dictating into right now, if any. */
export function recordingMessage(recording, messageId) {
  const remote = recording && recording.take && recording.take.remote;
  return Boolean(remote) && remote === messageId;
}

/**
 * Is this card shown in full? The take being recorded is always open, even
 * while every other card is closed and even after the person opened a long
 * older message.
 */
export function messageExpanded(recording, messageId, expandedId) {
  if (recordingMessage(recording, messageId)) return true;
  if (recording) return false;
  return expandedId === messageId;
}

export function buildFeed(messages) {
  const rows = [];
  let current = null;
  for (const message of sortAscending(messages)) {
    const key = dayKey(message.recorded);
    if (key !== current) {
      rows.push({ kind: 'day', key });
      current = key;
    }
    rows.push({ kind: 'message', message });
  }
  return rows;
}
