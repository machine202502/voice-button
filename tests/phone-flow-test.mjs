// The phone's pieces together: outbox + loop + sync.
//
//   node tests/phone-flow-test.mjs
//
// Each module has its own tests; this one checks that they work as one machine:
// a take recorded while offline survives, the connection comes back, the loop
// pushes it out, and the page is drawn exactly once with what the computer has.

import { createOutbox } from '../src/web/js/outbox.mjs';
import { createSync } from '../src/web/js/sync.mjs';
import { createLoop } from '../src/web/js/loop.mjs';
import { FLOOR_MS } from '../src/web/js/backoff.mjs';

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

function bag() {
  const map = new Map();
  return {
    map,
    async get(key) {
      return map.get(key);
    },
    async put(key, value) {
      map.set(key, value);
    },
    async update(key, change) {
      const next = change(map.get(key));
      if (next !== undefined) map.set(key, next);
    },
    async remove(key) {
      map.delete(key);
    },
    async keys() {
      return [...map.keys()];
    },
  };
}

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
    async fire() {
      const entry = [...jobs.entries()][0];
      if (!entry) return false;
      jobs.delete(entry[0]);
      await entry[1]();
      return true;
    },
  };
}

/** A computer that answers, and can be switched off. */
function fakeComputer() {
  const calls = [];
  let online = true;
  let nextId = 500;
  return {
    calls,
    get online() {
      return online;
    },
    set online(value) {
      online = value;
    },
    async call(method, path, body) {
      if (!online) throw new Error('offline');
      calls.push(method + ' ' + path);
      if (method === 'POST' && path === '/messages') return { id: nextId++ };
      if (method === 'GET') {
        return { messages: [{ id: 500, stage: 'queued', recorded: '2026-11-12T15:00:00+05:00' }] };
      }
      return { ok: true };
    },
  };
}

console.log('a take recorded offline goes out when the connection returns');
{
  const clock = fakeClock();
  const computer = fakeComputer();
  const store = bag();
  const chunks = bag();
  const outbox = createOutbox({ store, chunks, now: () => '2026-11-12T15:00:00+05:00' });
  let drawn = null;

  // The phone records while the computer is unreachable.
  computer.online = false;
  const take = outbox.newTake(1);
  await outbox.put(take);
  await outbox.putChunk(take.localId, 0, new Uint8Array([1, 2]));
  take.closed = true;
  await outbox.put(take);

  const sync = createSync({
    api: (method, path, body) => computer.call(method, path, body),
    outbox,
    render: async (page) => {
      drawn = page;
    },
    log: () => {},
  });
  sync.show(1);

  const loop = createLoop({
    tick: () => sync.tick(),
    schedule: (fn, delay) => clock.schedule(fn, delay),
    cancel: (id) => clock.cancel(id),
  });
  loop.start();
  await new Promise((resolve) => setTimeout(resolve, 5));
  check(store.map.size === 1, 'the take is still there after a failed attempt');
  check(clock.lastDelay > FLOOR_MS, 'and the loop slowed down: ' + clock.lastDelay);
  check(
    drawn === null || drawn.messages.length === 0,
    'nothing but the empty page was drawn from a dead connection',
  );

  // The connection comes back and the loop tries again.
  computer.online = true;
  await clock.fire();
  await new Promise((resolve) => setTimeout(resolve, 5));
  check(store.map.size === 0, 'the take left once the computer had it');
  check(
    computer.calls.includes('POST /messages/500/finish'),
    'the take was closed: ' + computer.calls.join('|'),
  );
  check(
    drawn && drawn.messages.length === 1,
    'the page was drawn with what the computer has',
  );
  check(clock.lastDelay === FLOOR_MS, 'and the pace is back to normal');
  loop.stop();
}

console.log('a take still being recorded is not closed behind the user');
{
  const clock = fakeClock();
  const computer = fakeComputer();
  const outbox = createOutbox({ store: bag(), chunks: bag(), now: () => '2026-11-12T15:00:00+05:00' });
  const sync = createSync({
    api: (method, path, body) => computer.call(method, path, body),
    outbox,
    render: async () => {},
    log: () => {},
  });
  sync.show(1);
  const take = outbox.newTake(1);
  await outbox.put(take);
  await outbox.putChunk(take.localId, 0, new Uint8Array([1, 2]));
  // The user is still talking: `closed` stays false.
  const loop = createLoop({
    tick: () => sync.tick(),
    schedule: (fn, delay) => clock.schedule(fn, delay),
    cancel: (id) => clock.cancel(id),
  });
  loop.start();
  await new Promise((resolve) => setTimeout(resolve, 5));
  check(
    !computer.calls.some((call) => call.endsWith('/finish')),
    'no finish while the user is recording: ' + computer.calls.join('|'),
  );
  check(computer.calls.includes('POST /messages/500/audio?seq=0'), 'but the audio does go out');
  loop.stop();
}

console.log('stopping the loop stops the work');
{
  const clock = fakeClock();
  const computer = fakeComputer();
  const outbox = createOutbox({ store: bag(), chunks: bag(), now: () => '2026-11-12T15:00:00+05:00' });
  const sync = createSync({
    api: (method, path, body) => computer.call(method, path, body),
    outbox,
    render: async () => {},
    log: () => {},
  });
  sync.show(1);
  const loop = createLoop({
    tick: () => sync.tick(),
    schedule: (fn, delay) => clock.schedule(fn, delay),
    cancel: (id) => clock.cancel(id),
  });
  loop.start();
  await new Promise((resolve) => setTimeout(resolve, 5));
  loop.stop();
  const callsAfterStop = computer.calls.length;
  await clock.fire();
  check(computer.calls.length === callsAfterStop, 'a stopped page makes no requests');
}

console.log('');
console.log(`${checks - failures}/${checks} checks passed`);
process.exit(failures === 0 ? 0 : 1);
