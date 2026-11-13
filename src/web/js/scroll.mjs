// Where a redraw leaves the scroll position.
//
// A chat opens at the newest message, but it must not drag the reader back to
// the bottom while they are reading older ones: the sync loop redraws about
// once a second, and every redraw used to jump.

/**
 * @param {object} options
 * @param {'keep'|'chat'} options.mode  `keep` for lists, `chat` for a monolog.
 * @param {boolean} options.first       the first draw after opening the chat
 * @param {boolean} options.atBottom    was the view at the bottom before redraw
 * @param {number} options.height       new content height
 * @param {number} options.viewport     visible height
 * @param {number} options.previous     scroll offset before redraw
 * @returns {number} scroll offset to use
 */
export function nextScroll({ mode = 'keep', first = false, atBottom = true, height = 0, viewport = 0, previous = 0 } = {}) {
  const bottom = Math.max(0, height - viewport);
  if (mode !== 'chat') return Math.min(previous, bottom);
  if (first || atBottom) return bottom;
  return Math.min(previous, bottom);
}

/** Is the view close enough to the bottom to count as "reading the newest"? */
export function atBottom({ offset = 0, height = 0, viewport = 0, slack = 80 } = {}) {
  return height - offset - viewport <= slack;
}
