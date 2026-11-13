// Tests for the page's single timer.
//
//   node tests/loop-test.mjs
//
// The scheduler is faked, so every case is checked without waiting: a success
// keeps the normal pace, a failure slows it down, a manual kick does not wait,
// and two runs never overlap.

import { createLoop } from '../src/web/js/loop.mjs';
import { FLOOR_MS, CEILING_MS } from '../src/web/js/backoff.mjs';

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

/** A scheduler the test drives by hand. */
function fakeClock() {
  const jobs = new Map();
  let next = 1;
  let lastDelay = null;
  return {
    jobs,
    get lastDelay() {
      return lastDelay;
    },
    schedule(fn, delay) {
      lastDelay = delay;
      const id = next++;
      jobs.set(id, fn);
      return id;
    },
    cancel(id) {
      jobs.delete(id);
    },
    /** Run the scheduled callback, as the browser would. */
    async fire() {
      const entry = [...jobs.entries()][0];
      if (!entry) return false;
      jobs.delete(entry[0]);
      await entry[1]();
      return true;
    },
  };
}

const flush = () => new Promise((resolve) => setTimeout(resolve, 0));

console.log('the first attempt does not wait');
{
  const clock = fakeClock();
  let ticks = 0;
  const loop = createLoop({
    tick: async () => {
      ticks += 1;
    },
    schedule: (fn, delay) => clock.schedule(fn, delay),
    cancel: (id) => clock.cancel(id),
  });
  loop.start();
  await flush();
  check(ticks === 1, 'one attempt right away: ' + ticks);
  check(clock.lastDelay === FLOOR_MS, 'then the normal pace: ' + clock.lastDelay);
  loop.stop();
}

console.log('a success keeps the normal pace');
{
  const clock = fakeClock();
  let ticks = 0;
  const loop = createLoop({
    tick: async () => {
      ticks += 1;
    },
    schedule: (fn, delay) => clock.schedule(fn, delay),
    cancel: (id) => clock.cancel(id),
  });
  loop.start();
  await flush();
  await clock.fire();
  await flush();
  check(ticks === 2, 'the second attempt ran: ' + ticks);
  check(clock.lastDelay === FLOOR_MS, 'pace unchanged: ' + clock.lastDelay);
  loop.stop();
}

console.log('a failure slows it down and a success restores it');
{
  const clock = fakeClock();
  const errors = [];
  let fail = true;
  const loop = createLoop({
    tick: async () => {
      if (fail) throw new Error('offline');
    },
    schedule: (fn, delay) => clock.schedule(fn, delay),
    cancel: (id) => clock.cancel(id),
    onError: (error) => errors.push(error.message),
  });
  loop.start();
  await flush();
  check(errors.length === 1, 'the failure was reported');
  check(clock.lastDelay > FLOOR_MS, 'and the pace slowed: ' + clock.lastDelay);
  await clock.fire();
  await flush();
  check(clock.lastDelay > FLOOR_MS, 'still slow while it keeps failing: ' + clock.lastDelay);
  fail = false;
  await clock.fire();
  await flush();
  check(clock.lastDelay === FLOOR_MS, 'back to normal once it works: ' + clock.lastDelay);
  loop.stop();
}

console.log('a kick does not wait for the timer');
{
  const clock = fakeClock();
  let ticks = 0;
  const loop = createLoop({
    tick: async () => {
      ticks += 1;
    },
    schedule: (fn, delay) => clock.schedule(fn, delay),
    cancel: (id) => clock.cancel(id),
  });
  loop.start();
  await flush();
  const before = ticks;
  loop.kick();
  await flush();
  check(ticks === before + 1, 'the kick ran the work at once');
  loop.stop();
}

console.log('two runs never overlap');
{
  const clock = fakeClock();
  let inside = 0;
  let worst = 0;
  const loop = createLoop({
    tick: async () => {
      inside += 1;
      worst = Math.max(worst, inside);
      await new Promise((resolve) => setTimeout(resolve, 5));
      inside -= 1;
    },
    schedule: (fn, delay) => clock.schedule(fn, delay),
    cancel: (id) => clock.cancel(id),
  });
  loop.start();
  loop.kick();
  loop.kick();
  await new Promise((resolve) => setTimeout(resolve, 20));
  check(worst === 1, 'only one attempt at a time: ' + worst);
  loop.stop();
}

console.log('stopping leaves nothing scheduled');
{
  const clock = fakeClock();
  const loop = createLoop({
    tick: async () => {},
    schedule: (fn, delay) => clock.schedule(fn, delay),
    cancel: (id) => clock.cancel(id),
  });
  loop.start();
  await flush();
  loop.stop();
  check(clock.jobs.size === 0, 'the timer was cancelled');
  check(loop.scheduled === false, 'and the page knows it');
  // A stopped loop ignores kicks, so a hidden page does no work.
  const before = clock.jobs.size;
  loop.kick();
  await flush();
  check(clock.jobs.size === before, 'a kick on a stopped loop does nothing');
}

console.log('the pause never exceeds the ceiling');
{
  const clock = fakeClock();
  const loop = createLoop({
    tick: async () => {
      throw new Error('offline');
    },
    schedule: (fn, delay) => clock.schedule(fn, delay),
    cancel: (id) => clock.cancel(id),
  });
  loop.start();
  await flush();
  for (let index = 0; index < 8; index += 1) {
    await clock.fire();
    await flush();
  }
  check(clock.lastDelay <= CEILING_MS, 'the ceiling holds: ' + clock.lastDelay);
  loop.stop();
}

console.log('');
console.log(`${checks - failures}/${checks} checks passed`);
process.exit(failures === 0 ? 0 : 1);
