// The changes the phone made that the computer has not acknowledged yet.
//
// One ordered queue, next to the recording outbox and with the same rules:
//
//   * the order is the order the person acted in, and it never changes;
//   * an entry leaves only after the computer accepted it, or refused it for
//     good, so a dead connection resumes instead of losing an action;
//   * nothing here touches the network or the DOM: the store is injected, so
//     the same code runs on IndexedDB in the phone and on a map in the tests.
//
// The store is the phone's `mutations` object store. It assigns `seq` itself
// (`autoIncrement` on the `seq` key path), so a record is appended without a
// key and the store's answer is the number that orders it.

import type { MutationStore } from './storage';
import type { Mutation, MutationInput, MutationRecord } from './types';

export interface CreateMutationsOptions {
  store: MutationStore;
  /** The clock the queue stamps entries with; the tests set it. */
  now?: () => string;
}

/** The queue itself: one order, no network, no DOM. */
export interface MutationQueue {
  /** Append an action and return it with the `seq` the store gave it. */
  put(mutation: MutationInput): Promise<Mutation>;
  /** Everything waiting, oldest first. */
  list(): Promise<Mutation[]>;
  /** The oldest entry, without removing it; `null` when the queue is empty. */
  take(): Promise<Mutation | null>;
  /** Drop one acknowledged (or refused) entry. */
  remove(seq: number): Promise<void>;
  /** How many entries are waiting. */
  count(): Promise<number>;
}

export function createMutations({
  store,
  now = () => new Date().toISOString(),
}: CreateMutationsOptions): MutationQueue {
  async function all(): Promise<Mutation[]> {
    const keys = await store.keys();
    const found: Mutation[] = [];
    for (const key of keys) {
      const seq = Number(key);
      if (!Number.isFinite(seq)) continue;
      const record = await store.get(seq);
      if (!record) continue;
      // The key is the order; a stored copy may carry an older one.
      found.push({ ...record, seq });
    }
    // IndexedDB returns keys in order; a map need not, and the order is the
    // whole point of this queue.
    found.sort((left, right) => left.seq - right.seq);
    return found;
  }

  return {
    async put(mutation) {
      const record: MutationRecord = {
        kind: mutation.kind,
        target: mutation.target,
        payload: mutation.payload,
        local: mutation.local === true,
        createdAt: mutation.createdAt ?? now(),
      };
      const seq = await store.put(record);
      return { ...record, seq };
    },

    list: all,

    async take() {
      const found = await all();
      return found.length > 0 ? found[0] ?? null : null;
    },

    async remove(seq) {
      await store.remove(Number(seq));
    },

    async count() {
      return (await store.keys()).length;
    },
  };
}
