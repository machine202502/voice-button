// Tests for the phone's outbox.
//
//   vitest run src/data
//
// The outbox is what makes a recording survive a dead connection, so the cases
// here are the ones that used to lose audio: a chunk dropped too early, a take
// removed before the computer had it, and audio arriving out of order.
//
// Ported from `tests/outbox-test.mjs`; the bags below have the same surface as
// the IndexedDB adapter on the phone.

import { describe, expect, it } from 'vitest';

import { createOutbox } from './outbox';
import type { Outbox } from './outbox';
import type { StoreBag } from './storage';
import type { Take } from './types';

// ---------------------------------------------------------------- fakes

/** One key/value bag with the phone store's surface, over a plain map. */
class MapBag<T> implements StoreBag<T> {
  readonly map = new Map<IDBValidKey, T>();

  async get(key: IDBValidKey): Promise<T | undefined> {
    return this.map.get(key);
  }

  async put(value: T): Promise<IDBValidKey>;
  async put(key: IDBValidKey, value: T): Promise<IDBValidKey>;
  async put(keyOrValue: IDBValidKey | T, value?: T): Promise<IDBValidKey> {
    if (value === undefined) {
      // The outbox only ever writes chunks under an explicit key.
      throw new Error('this bag stores values under an explicit key');
    }
    const key = keyOrValue as IDBValidKey;
    this.map.set(key, value);
    return key;
  }

  async update(
    key: IDBValidKey,
    change: (previous: T | undefined) => T | undefined,
  ): Promise<void> {
    const next = change(this.map.get(key));
    if (next !== undefined) this.map.set(key, next);
  }

  async remove(key: IDBValidKey): Promise<void> {
    this.map.delete(key);
  }

  async keys(): Promise<IDBValidKey[]> {
    return [...this.map.keys()];
  }
}

function outboxWithClock(): {
  outbox: Outbox;
  store: MapBag<Take>;
  chunks: MapBag<Uint8Array>;
} {
  let tick = 0;
  const store = new MapBag<Take>();
  const chunks = new MapBag<Uint8Array>();
  return {
    outbox: createOutbox({
      store,
      chunks,
      now: () => '2026-11-12T15:0' + String(tick++) + ':00+05:00',
    }),
    store,
    chunks,
  };
}

/** The one take in the outbox; fails the test when there is not exactly one. */
async function only(outbox: Outbox): Promise<Take> {
  const all = await outbox.list();
  const found = all[0];
  if (all.length !== 1 || !found) throw new Error('expected one take, found ' + all.length);
  return found;
}

// ---------------------------------------------------------------- tests

describe('the recording outbox', () => {
  it('keeps a take until the computer has it', async () => {
    const { outbox } = outboxWithClock();
    const take = outbox.newTake(1);
    await outbox.put(take);
    expect((await outbox.list()).length).toBe(1);
    await outbox.remove(take.localId);
    expect((await outbox.list()).length).toBe(0);
  });

  it('sends audio in the order it was recorded', async () => {
    const { outbox } = outboxWithClock();
    const take = outbox.newTake(1);
    await outbox.put(take);
    await outbox.putChunk(take.localId, 0, new Uint8Array([1]));
    await outbox.putChunk(take.localId, 2, new Uint8Array([3]));
    await outbox.putChunk(take.localId, 1, new Uint8Array([2]));
    const order = (await outbox.chunks(take.localId)).map((chunk) => chunk.seq);
    expect(order.join(',')).toBe('0,1,2');
  });

  it('does not take the other chunks with a dropped one', async () => {
    const { outbox } = outboxWithClock();
    const take = outbox.newTake(1);
    await outbox.put(take);
    await outbox.putChunk(take.localId, 0, new Uint8Array([1]));
    await outbox.putChunk(take.localId, 1, new Uint8Array([2]));
    await outbox.dropChunk(take.localId, 0);
    const left = await outbox.chunks(take.localId);
    expect(left.length).toBe(1);
    expect(left[0]?.seq).toBe(1);
    expect(await outbox.nextSeq(take.localId)).toBe(2);
  });

  it('lists takes oldest recording first', async () => {
    const { outbox } = outboxWithClock();
    const first: Take = {
      localId: 'b',
      monolog: 1,
      recorded: '2026-11-12T15:10:00+05:00',
      remote: 2,
      closed: true,
      seq: 0,
    };
    const second: Take = {
      localId: 'a',
      monolog: 1,
      recorded: '2026-11-12T15:05:00+05:00',
      remote: 1,
      closed: true,
      seq: 0,
    };
    await outbox.put(first);
    await outbox.put(second);
    const order = (await outbox.list()).map((take) => take.localId);
    expect(order.join(',')).toBe('a,b');
  });

  it('does not let the recorder erase the id the computer gave', async () => {
    const { outbox } = outboxWithClock();
    const take = outbox.newTake(1);
    await outbox.put(take);
    // The sync loop reads the take, gets an id for it and stores it back.
    const listed = await only(outbox);
    listed.remote = 42;
    await outbox.put(listed);
    // The recorder now stores the take it holds — the one that never saw the id.
    take.seq = 3;
    await outbox.put(take);
    const after = await only(outbox);
    expect(after.remote).toBe(42);
    expect(after.seq).toBe(3);
  });

  it('does not reopen a stopped take with a stale copy', async () => {
    const { outbox } = outboxWithClock();
    const take = outbox.newTake(1);
    await outbox.put(take);
    // A copy read before the user pressed stop.
    const stale = await only(outbox);
    take.seq = 5;
    take.closed = true;
    await outbox.put(take);
    // The sync loop writes that old copy back after assigning the id.
    stale.remote = 7;
    await outbox.put(stale);
    const after = await only(outbox);
    expect(after.closed).toBe(true);
    expect(after.remote).toBe(7);
    expect(after.seq).toBe(5);
  });

  it('removes a take together with its audio', async () => {
    const { outbox, chunks } = outboxWithClock();
    const take = outbox.newTake(1);
    await outbox.put(take);
    await outbox.putChunk(take.localId, 0, new Uint8Array([1]));
    await outbox.putChunk(take.localId, 1, new Uint8Array([2]));
    await outbox.remove(take.localId);
    expect(chunks.map.size).toBe(0);
  });

  it('gives a new take its own identity and time', async () => {
    const { outbox } = outboxWithClock();
    const take = outbox.newTake(7);
    expect(typeof take.localId).toBe('string');
    expect(take.localId.length).toBeGreaterThan(8);
    expect(take.monolog).toBe(7);
    expect(take.remote).toBeNull();
    expect(take.closed).toBe(false);
    expect(take.recorded.startsWith('2026-11-12')).toBe(true);
    const other = outbox.newTake(7);
    expect(other.localId).not.toBe(take.localId);
  });
});
