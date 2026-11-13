// What the phone has recorded but the computer has not acknowledged yet.
//
// Two things live here: the takes themselves and their audio chunks. The rules
// the page depends on:
//
//   * a take is only removed after the computer said it has it;
//   * chunks go out in the order they were recorded, and each one is dropped
//     only after its own acknowledgement, so a broken connection resumes where
//     it stopped instead of resending everything;
//   * the storage is injected, so the same code runs on IndexedDB in the phone
//     and on a plain map in the tests.
//
// Ported from `src/web/js/outbox.mjs`. The bags are the `takes` and `audio`
// object stores that `openStorage()` returns; this module decides nothing about
// the network or the DOM.

import type { StoreBag } from './storage';
import type { Chunk, Take } from './types';

export interface CreateOutboxOptions {
  /** The `takes` store: one record per unfinished recording, keyed by `localId`. */
  store: StoreBag<Take>;
  /** The `audio` store: one record per chunk, keyed by `localId#seq`. */
  chunks: StoreBag<Uint8Array>;
  /** The phone clock the new takes are stamped with; the tests set it. */
  now?: () => string;
}

/** The outbox: the takes and their audio, until the computer has both. */
export interface Outbox {
  /** Every take waiting, oldest recording first. */
  list(): Promise<Take[]>;
  /** Store a take, merging it into what is already stored. */
  put(take: Take): Promise<void>;
  /** Drop a take and all of its audio; only after the computer has it. */
  remove(localId: string): Promise<void>;
  /** Audio of one take, in the order it was recorded. */
  chunks(localId: string): Promise<Chunk[]>;
  /** Store one chunk of a take under its number. */
  putChunk(localId: string, seq: number, bytes: Uint8Array): Promise<void>;
  /** Drop one acknowledged chunk. */
  dropChunk(localId: string, seq: number): Promise<void>;
  /** Next free number for a chunk of this take. */
  nextSeq(localId: string): Promise<number>;
  /** A take that starts now, with an id of its own. */
  newTake(monolog: number): Take;
}

/** The key of one chunk in the audio store. */
function chunkKey(localId: string, seq: number): string {
  return localId + '#' + seq;
}

/** An id no other take has: the platform's UUID, or time plus randomness. */
function takeId(): string {
  const generator = globalThis.crypto;
  if (generator && typeof generator.randomUUID === 'function') {
    return generator.randomUUID();
  }
  return String(Date.now()) + Math.random().toString(16).slice(2);
}

export function createOutbox({
  store,
  chunks,
  now = () => new Date().toISOString(),
}: CreateOutboxOptions): Outbox {
  /** Audio of one take, in the order it was recorded. */
  async function chunksOf(localId: string): Promise<Chunk[]> {
    const prefix = localId + '#';
    const found: Chunk[] = [];
    for (const key of await chunks.keys()) {
      if (typeof key !== 'string' || !key.startsWith(prefix)) continue;
      const bytes = await chunks.get(key);
      if (!bytes) continue;
      found.push({ seq: Number(key.slice(prefix.length)) || 0, bytes });
    }
    found.sort((left, right) => left.seq - right.seq);
    return found;
  }

  return {
    /** Every take waiting, oldest recording first. */
    async list() {
      const takes: Take[] = [];
      for (const key of await store.keys()) {
        const take = await store.get(key);
        if (take) takes.push(take);
      }
      takes.sort((left, right) => {
        const a = String(left.recorded || '');
        const b = String(right.recorded || '');
        return a < b ? -1 : a > b ? 1 : 0;
      });
      return takes;
    },

    async put(take) {
      // Only plain data: a take may carry live objects (a wake lock, a stream)
      // and IndexedDB refuses to clone those, which silently loses the take.
      //
      // The record is merged with what is stored, in one transaction, because
      // the page writes a take from two places: the recorder owns `seq` and
      // `closed`, the sync loop owns `remote`. A plain overwrite let each of
      // them erase the other's field — the take forgot the id the computer had
      // given it and every tick created a brand new message, and a stop was
      // undone by a copy read before it.
      await store.update(take.localId, (previous) => ({
        localId: take.localId,
        monolog: take.monolog,
        recorded: take.recorded,
        // An id is given once: a copy of the take that never saw it keeps it.
        remote: take.remote ?? previous?.remote ?? null,
        // A take is stopped once and stays stopped.
        closed: take.closed === true || previous?.closed === true,
        // Only the recorder counts chunks, and it only counts up.
        seq: Math.max(take.seq, previous?.seq ?? 0),
      }));
    },

    async remove(localId) {
      await store.remove(localId);
      const prefix = localId + '#';
      for (const key of await chunks.keys()) {
        if (typeof key === 'string' && key.startsWith(prefix)) await chunks.remove(key);
      }
    },

    chunks: chunksOf,

    async putChunk(localId, seq, bytes) {
      const number = Number(seq);
      if (!Number.isFinite(number) || number < 0) {
        throw new Error('a chunk needs a number: ' + String(seq));
      }
      await chunks.put(chunkKey(localId, number), bytes);
    },

    async dropChunk(localId, seq) {
      await chunks.remove(chunkKey(localId, Number(seq)));
    },

    /** Next free number for a chunk of this take. */
    async nextSeq(localId) {
      const found = await chunksOf(localId);
      const last = found[found.length - 1];
      return last ? last.seq + 1 : 0;
    },

    /** A take that starts now, with an id of its own. */
    newTake(monolog) {
      return {
        localId: takeId(),
        monolog,
        recorded: now(),
        remote: null,
        closed: false,
        // The next chunk number. Without it the page wrote `NaN` as an
        // IndexedDB key, which the store rejects, so no audio ever left.
        seq: 0,
      };
    },
  };
}
