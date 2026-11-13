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

/** `store` and `chunks` are minimal key/value bags: get, put, remove, keys. */
export function createOutbox({ store, chunks, now = () => new Date().toISOString() }) {
  const chunkKey = (localId, seq) => localId + '#' + seq;

  return {
    /** Every take waiting, oldest recording first. */
    async list() {
      const takes = [];
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
      await store.update(take.localId, (previous) => {
        const older = previous || {};
        return {
          localId: take.localId,
          monolog: take.monolog ?? older.monolog,
          recorded: take.recorded ?? older.recorded,
          // An id is given once: a copy of the take that never saw it keeps it.
          remote: take.remote ?? older.remote ?? null,
          // A take is stopped once and stays stopped.
          closed: take.closed === true || older.closed === true,
          // Only the recorder counts chunks, and it only counts up.
          seq: Math.max(take.seq ?? 0, older.seq ?? 0),
        };
      });
    },

    async remove(localId) {
      await store.remove(localId);
      for (const key of await chunks.keys()) {
        if (key.startsWith(localId + '#')) await chunks.remove(key);
      }
    },

    /** Audio of one take, in the order it was recorded. */
    async chunks(localId) {
      const found = [];
      for (const key of await chunks.keys()) {
        if (!key.startsWith(localId + '#')) continue;
        const bytes = await chunks.get(key);
        if (bytes) found.push({ seq: Number(key.split('#')[1] || 0), bytes });
      }
      found.sort((left, right) => left.seq - right.seq);
      return found;
    },

    async putChunk(localId, seq, bytes) {
      const number = Number(seq);
      if (!Number.isFinite(number) || number < 0) {
        throw new Error('a chunk needs a number: ' + seq);
      }
      await chunks.put(chunkKey(localId, number), bytes);
    },

    async dropChunk(localId, seq) {
      await chunks.remove(chunkKey(localId, seq));
    },

    /** Next free number for a chunk of this take. */
    async nextSeq(localId) {
      const found = await this.chunks(localId);
      return found.length === 0 ? 0 : found[found.length - 1].seq + 1;
    },

    /** A take that starts now, with an id of its own. */
    newTake(monolog) {
      return {
        localId:
          (globalThis.crypto && globalThis.crypto.randomUUID
            ? globalThis.crypto.randomUUID()
            : String(Date.now()) + Math.random().toString(16).slice(2)),
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
