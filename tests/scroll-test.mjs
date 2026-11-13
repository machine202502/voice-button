// Scroll position after a redraw. Run with: node tests/scroll-test.mjs

import { atBottom, nextScroll } from '../src/web/js/scroll.mjs';

let checks = 0;
const failures = [];

function check(condition, message) {
  checks += 1;
  if (condition) {
    console.log('  ok    ' + message);
  } else {
    failures.push(message);
    console.log('  FAIL  ' + message);
  }
}

// A chat opens on the newest message.
check(
  nextScroll({ mode: 'chat', first: true, atBottom: true, height: 2000, viewport: 600, previous: 0 }) === 1400,
  'a freshly opened chat starts at the bottom',
);

// Reading older messages must survive the once-a-second redraw.
check(
  nextScroll({ mode: 'chat', first: false, atBottom: false, height: 2400, viewport: 600, previous: 120 }) === 120,
  'a reader who scrolled up is not dragged down',
);

// Following new messages while at the bottom stays at the bottom.
check(
  nextScroll({ mode: 'chat', first: false, atBottom: true, height: 2600, viewport: 600, previous: 1800 }) === 2000,
  'a reader at the bottom follows new messages',
);

// Lists keep their place too.
check(
  nextScroll({ mode: 'keep', height: 1200, viewport: 600, previous: 90 }) === 90,
  'a list keeps its scroll position',
);
check(
  nextScroll({ mode: 'keep', height: 700, viewport: 600, previous: 500 }) === 100,
  'a shorter list clamps the old offset',
);

check(
  atBottom({ offset: 1400, height: 2000, viewport: 600 }) === true,
  'exactly at the bottom counts as the bottom',
);
check(
  atBottom({ offset: 1000, height: 2000, viewport: 600 }) === false,
  'middle of a long chat does not',
);
check(
  atBottom({ offset: 0, height: 400, viewport: 600 }) === true,
  'content shorter than the viewport is always at the bottom',
);

console.log('');
if (failures.length > 0) {
  console.log('failed:');
  for (const failure of failures) console.log('  - ' + failure);
  process.exit(1);
}
console.log(`${checks}/${checks} checks passed`);
