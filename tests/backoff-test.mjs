// Tests for the phone's retry pace.
//
//   node tests/backoff-test.mjs
//
// A phone out of range must not hammer the air, and the first success must put
// it back on the normal pace at once.

import { FLOOR_MS, CEILING_MS, delayFor, createBackoff } from '../src/web/js/backoff.mjs';

let failures = 0;
let checks = 0;

function check(condition, message) {
  checks += 1;
  if (condition) {
    console.log('  ok    ' + message);
  } else {
    console.log('  FAIL  ' + message);
    failures += 1;
  }
}

console.log('a working connection polls at the normal pace');
check(delayFor(0) === FLOOR_MS, 'no failures means one second');
check(delayFor(-3) === FLOOR_MS, 'a nonsense count is treated as none');
check(delayFor(NaN) === FLOOR_MS, 'a broken count is treated as none');

console.log('each failure slows the page down');
const first = delayFor(1);
const second = delayFor(2);
const third = delayFor(3);
check(first > FLOOR_MS, 'the first failure already waits longer: ' + first);
check(second > first, 'and the next one longer still: ' + second);
check(third > second, 'and again: ' + third);

console.log('the ceiling holds');
check(delayFor(8) <= CEILING_MS, 'never longer than the ceiling');
check(delayFor(50) === CEILING_MS, 'a long outage settles at the ceiling');
check(CEILING_MS <= 30_000, 'the ceiling is still quick enough to notice recovery');

console.log('a success puts it back at once');
{
  const backoff = createBackoff();
  backoff.failure();
  backoff.failure();
  backoff.failure();
  check(backoff.failures === 3, 'three failures are counted');
  check(backoff.current() > FLOOR_MS, 'and the pace has slowed');
  const after = backoff.success();
  check(after === FLOOR_MS, 'a success returns to one second');
  check(backoff.failures === 0, 'the count is cleared');
  check(backoff.current() === FLOOR_MS, 'and the pace stays there');
}

console.log('coming back to the app tries at once');
{
  const backoff = createBackoff();
  for (let index = 0; index < 5; index += 1) backoff.failure();
  check(backoff.current() === CEILING_MS, 'a long outage is at the ceiling');
  check(backoff.reset() === FLOOR_MS, 'opening the page again waits nothing extra');
  check(backoff.failures === 0, 'and the count starts from zero');
}

console.log('the delay never jumps backwards while failing');
{
  const backoff = createBackoff();
  let previous = 0;
  for (let index = 0; index < 10; index += 1) {
    const delay = backoff.failure();
    check(delay >= previous, 'failure ' + (index + 1) + ' waits at least as long: ' + delay);
    previous = delay;
  }
}

console.log('');
console.log(`${checks - failures}/${checks} checks passed`);
process.exit(failures === 0 ? 0 : 1);
