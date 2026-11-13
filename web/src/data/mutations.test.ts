// Tests for the queue of manual changes, without a browser and without a
// network.
//
//   vitest run src/data
//
// The queue is what makes a rename, an archive, an edit and a delete survive a
// dead connection, so the cases here are the ones that would lose an action:
// the order, an entry taken before an earlier one, and the queue surviving a
// reload.

import { describe, expect, it } from 'vitest';

import { createMutations } from './mutations';
import type { MutationStore } from './storage';
import type { Mutation, MutationRecord } from './types';

/** The phone's `mutations` store as a map: it assigns `seq` itself. */
function fakeStore(): MutationStore & { rows: Map<number, MutationRecord> } {
  const rows = new Map<number, MutationRecord>();
  let next = 1;
  return {
    rows,
    async put(value: MutationRecord): Promise<number> {
      const seq = next;
      next += 1;
      rows.set(seq, { ...value });
      return seq;
    },
    async get(key: number): Promise<MutationRecord | undefined> {
      return rows.get(key);
    },
    async remove(key: number): Promise<void> {
      rows.delete(key);
    },
    async keys(): Promise<unknown[]> {
      // Backwards, so a queue that trusted the store instead of sorting fails.
      return [...rows.keys()].reverse();
    },
  };
}

function queueWithClock() {
  let tick = 0;
  return createMutations({
    store: fakeStore(),
    now: () => '2026-11-13T10:0' + tick++ + ':00+05:00',
  });
}

describe('the change queue', () => {
  it('appends an action and returns the seq the store gave it', async () => {
    const queue = queueWithClock();
    const stored = await queue.put({
      kind: 'monolog.rename',
      target: { monolog: 12 },
      payload: { name: 'Дом' },
      local: false,
      // The caller cannot choose the order: the store does.
      seq: 99,
    });
    expect(stored.seq).toBe(1);
    expect(stored.kind).toBe('monolog.rename');
    expect(stored.target).toEqual({ monolog: 12 });
    expect(stored.payload).toEqual({ name: 'Дом' });
    expect(stored.local).toBe(false);
    expect(stored.createdAt).toBe('2026-11-13T10:00:00+05:00');
  });

  it('keeps the order the person acted in', async () => {
    const queue = queueWithClock();
    await queue.put({
      kind: 'monolog.rename',
      target: { monolog: 1 },
      payload: { name: 'Первый' },
      local: false,
    });
    await queue.put({
      kind: 'monolog.archive',
      target: { monolog: 2 },
      payload: { archived: true },
      local: false,
    });
    await queue.put({
      kind: 'message.edit',
      target: { monolog: 2, message: 34 },
      payload: { text: 'Новый текст' },
      local: false,
    });
    const list = await queue.list();
    expect(list.map((entry) => entry.seq)).toEqual([1, 2, 3]);
    expect(list.map((entry) => entry.kind)).toEqual([
      'monolog.rename',
      'monolog.archive',
      'message.edit',
    ]);
    expect(await queue.count()).toBe(3);
  });

  it('takes the oldest entry without removing it', async () => {
    const queue = queueWithClock();
    await queue.put({
      kind: 'message.delete',
      target: { monolog: 1, message: 5 },
      payload: {},
      local: false,
    });
    await queue.put({
      kind: 'monolog.rename',
      target: { monolog: 2 },
      payload: { name: 'Второй' },
      local: false,
    });
    const first = await queue.take();
    expect(first?.seq).toBe(1);
    expect(first?.kind).toBe('message.delete');
    // Taking again still hands out the same entry: `take` is a look, not a pop.
    expect((await queue.take())?.seq).toBe(1);
    expect(await queue.count()).toBe(2);
  });

  it('never hands out a later action before an earlier one', async () => {
    const queue = queueWithClock();
    for (const name of ['один', 'два', 'три']) {
      await queue.put({
        kind: 'monolog.rename',
        target: { monolog: 7 },
        payload: { name },
        local: false,
      });
    }
    const seen: number[] = [];
    for (;;) {
      const next = await queue.take();
      if (!next) break;
      seen.push(next.seq);
      await queue.remove(next.seq);
    }
    expect(seen).toEqual([1, 2, 3]);
    expect(await queue.count()).toBe(0);
    expect(await queue.take()).toBeNull();
  });

  it('drops exactly one entry', async () => {
    const queue = queueWithClock();
    const names = ['a', 'b', 'c'];
    for (const name of names) {
      await queue.put({
        kind: 'monolog.rename',
        target: { monolog: 3 },
        payload: { name },
        local: false,
      });
    }
    await queue.remove(2);
    const left = await queue.list();
    expect(left.map((entry) => entry.seq)).toEqual([1, 3]);
    expect(await queue.count()).toBe(2);
  });

  it('keeps the queue across a reload', async () => {
    const store = fakeStore();
    const before = createMutations({ store, now: () => '2026-11-13T10:00:00+05:00' });
    await before.put({
      kind: 'monolog.archive',
      target: { monolog: 4 },
      payload: { archived: true },
      local: false,
    });
    // A new page load opens the same store: what was queued is still queued,
    // and the next action goes after it.
    const after = createMutations({ store, now: () => '2026-11-13T10:05:00+05:00' });
    const kept: Mutation[] = await after.list();
    expect(kept.map((entry) => entry.kind)).toEqual(['monolog.archive']);
    const appended = await after.put({
      kind: 'message.delete',
      target: { monolog: 4, message: 9 },
      payload: {},
      local: false,
    });
    expect(appended.seq).toBe(2);
  });

  it('keeps a local target marked local', async () => {
    const queue = queueWithClock();
    const stored = await queue.put({
      kind: 'monolog.rename',
      target: { monolog: -1 },
      payload: { name: 'Офлайн' },
      local: true,
    });
    expect(stored.local).toBe(true);
    expect(stored.target.monolog).toBe(-1);
    const [waiting] = await queue.list();
    expect(waiting?.local).toBe(true);
  });
});
