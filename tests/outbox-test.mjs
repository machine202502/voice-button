// Tests for the phone's outbox.
//
//   node tests/outbox-test.mjs
//
// The outbox is what makes a recording survive a dead connection, so the cases
// here are the ones that used to lose audio: a chunk dropped too early, a take
// removed before the computer had it, and audio arriving out of order.

import { createOutbox } from '../src/web/js/outbox.mjs';

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

/** A bag with the same surface as the IndexedDB wrapper on the phone. */
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

function outboxWithClock() {
  let tick = 0;
  const store = bag();
  const chunks = bag();
  return {
    outbox: createOutbox({
      store,
      chunks,
      now: () => '2026-11-12T15:0' + tick++ + ':00+05:00',
    }),
    store,
    chunks,
  };
}

console.log('a take is kept until the computer has it');
{
  const { outbox } = outboxWithClock();
  const take = outbox.newTake(1);
  await outbox.put(take);
  check((await outbox.list()).length === 1, 'the take waits in the outbox');
  await outbox.remove(take.localId);
  check((await outbox.list()).length === 0, 'and leaves when it is removed');
}

console.log('audio goes out in the order it was recorded');
{
  const { outbox } = outboxWithClock();
  const take = outbox.newTake(1);
  await outbox.put(take);
  await outbox.putChunk(take.localId, 0, new Uint8Array([1]));
  await outbox.putChunk(take.localId, 2, new Uint8Array([3]));
  await outbox.putChunk(take.localId, 1, new Uint8Array([2]));
  const order = (await outbox.chunks(take.localId)).map((chunk) => chunk.seq);
  check(order.join(',') === '0,1,2', 'sorted by number, not by arrival: ' + order.join(','));
}

console.log('a dropped chunk does not take the others with it');
{
  const { outbox } = outboxWithClock();
  const take = outbox.newTake(1);
  await outbox.put(take);
  await outbox.putChunk(take.localId, 0, new Uint8Array([1]));
  await outbox.putChunk(take.localId, 1, new Uint8Array([2]));
  await outbox.dropChunk(take.localId, 0);
  const left = await outbox.chunks(take.localId);
  check(left.length === 1 && left[0].seq === 1, 'only the acknowledged chunk is gone');
  check((await outbox.nextSeq(take.localId)) === 2, 'the next number continues after the last one');
}

console.log('takes are listed oldest recording first');
{
  const { outbox } = outboxWithClock();
  const first = { localId: 'b', monolog: 1, recorded: '2026-11-12T15:10:00+05:00', remote: 2, closed: true };
  const second = { localId: 'a', monolog: 1, recorded: '2026-11-12T15:05:00+05:00', remote: 1, closed: true };
  await outbox.put(first);
  await outbox.put(second);
  const order = (await outbox.list()).map((take) => take.localId);
  check(order.join(',') === 'a,b', 'by the phone clock: ' + order.join(','));
}

console.log('the recorder does not erase the id the computer gave');
{
  const { outbox } = outboxWithClock();
  const take = outbox.newTake(1);
  await outbox.put(take);
  // The sync loop reads the take, gets an id for it and stores it back.
  const listed = (await outbox.list())[0];
  listed.remote = 42;
  await outbox.put(listed);
  // The recorder now stores the take it holds — the one that never saw the id.
  take.seq = 3;
  await outbox.put(take);
  const after = (await outbox.list())[0];
  check(after.remote === 42, 'the id from the computer survives a chunk write');
  check(after.seq === 3, 'and the chunk counter comes from the recorder');
}

console.log('a stopped take is not reopened by a stale copy');
{
  const { outbox } = outboxWithClock();
  const take = outbox.newTake(1);
  await outbox.put(take);
  // A copy read before the user pressed stop.
  const stale = (await outbox.list())[0];
  take.seq = 5;
  take.closed = true;
  await outbox.put(take);
  // The sync loop writes that old copy back after assigning the id.
  stale.remote = 7;
  await outbox.put(stale);
  const after = (await outbox.list())[0];
  check(after.closed === true, 'the stop is not undone by the sync loop');
  check(after.remote === 7, 'and the id assigned after the stop is kept');
  check(after.seq === 5, 'the chunk counter never goes backwards');
}

console.log('removing a take also removes its audio');
{
  const { outbox, chunks } = outboxWithClock();
  const take = outbox.newTake(1);
  await outbox.put(take);
  await outbox.putChunk(take.localId, 0, new Uint8Array([1]));
  await outbox.putChunk(take.localId, 1, new Uint8Array([2]));
  await outbox.remove(take.localId);
  check(chunks.map.size === 0, 'no orphan audio is left behind');
}

console.log('a new take carries its own identity and time');
{
  const { outbox } = outboxWithClock();
  const take = outbox.newTake(7);
  check(typeof take.localId === 'string' && take.localId.length > 8, 'it has an id');
  check(take.monolog === 7, 'it belongs to the monolog that was open');
  check(take.remote === null && take.closed === false, 'and no id on the computer yet');
  check(take.recorded.startsWith('2026-11-12'), 'the phone clock is used: ' + take.recorded);
  const other = outbox.newTake(7);
  check(other.localId !== take.localId, 'two takes never share an id');
}

console.log('');
console.log(`${checks - failures}/${checks} checks passed`);
process.exit(failures === 0 ? 0 : 1);
