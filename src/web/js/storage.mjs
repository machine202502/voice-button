// The phone's storage: IndexedDB behind the little bag interface the outbox
// expects (`get`, `put`, `remove`, `keys`) and a paged store for messages.
//
// Kept separate from the outbox so the outbox itself is testable with a map and
// runs unchanged on the phone. Nothing here decides anything: it only moves
// values in and out.
//
// Messages are one record per message, keyed by
// `['monolog', 'recorded', 'id']`. That key is the cursor the API uses, and it
// is the order the feed is read in: a page is a key range, never a whole array.
// The old store held one array per monolog; it is dropped on the version bump
// and never migrated.

const DB_NAME = 'voice-button';
const VERSION = 4;
const MESSAGES = 'messages';
export const MESSAGE_KEY_PATH = ['monolog', 'recorded', 'id'];

/** The key of one stored message; the same `recorded|id` order the API uses. */
export function messageKey(monolog, recorded, id) {
  return [Number(monolog), String(recorded ?? ''), Number(id)];
}

/** Open (or create) the database. */
export function openDatabase(factory = indexedDB) {
  return new Promise((resolve, reject) => {
    const request = factory.open(DB_NAME, VERSION);
    request.onupgradeneeded = () => {
      const db = request.result;
      // The old page used "outbox" and "chunks"; the names stay so a phone that
      // already has recordings keeps them.
      if (!db.objectStoreNames.contains('takes')) db.createObjectStore('takes');
      if (!db.objectStoreNames.contains('audio')) db.createObjectStore('audio');
      if (!db.objectStoreNames.contains('outbox')) db.createObjectStore('outbox');
      if (!db.objectStoreNames.contains('chunks')) db.createObjectStore('chunks');
      // What the page can still show without the computer: the monologs
      // (negative keys for ones made here).
      if (!db.objectStoreNames.contains('monologs')) db.createObjectStore('monologs');
      // The feed is durable but read in pages. An older `messages` store held
      // one array per monolog; it is deleted, not migrated: no page can read it
      // and the phone will be reinstalled.
      if (db.objectStoreNames.contains(MESSAGES)) db.deleteObjectStore(MESSAGES);
      db.createObjectStore(MESSAGES, { keyPath: MESSAGE_KEY_PATH });
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

/** One object store as a bag. */
export function storeBag(db, name) {
  const run = (mode, action) =>
    new Promise((resolve, reject) => {
      let request;
      try {
        const transaction = db.transaction(name, mode);
        request = action(transaction.objectStore(name));
        transaction.oncomplete = () => resolve(request ? request.result : undefined);
        transaction.onerror = () => reject(transaction.error);
        transaction.onabort = () => reject(transaction.error || new Error('aborted'));
      } catch (error) {
        reject(error);
      }
    });

  return {
    get: (key) => run('readonly', (store) => store.get(key)),
    put: (key, value) => run('readwrite', (store) => store.put(value, key)),
    remove: (key) => run('readwrite', (store) => store.delete(key)),
    keys: async () => {
      const keys = await run('readonly', (store) => store.getAllKeys());
      return Array.isArray(keys) ? keys : [];
    },
    /**
     * Change one value from what is stored right now, in one transaction.
     *
     * A plain get followed by a put is two transactions: another writer can
     * slip between them and its field is lost. That is exactly how the phone
     * used to forget the message id the computer gave the take, and made a new
     * message for every chunk.
     */
    update: (key, change) =>
      new Promise((resolve, reject) => {
        try {
          const transaction = db.transaction(name, 'readwrite');
          const store = transaction.objectStore(name);
          const read = store.get(key);
          read.onsuccess = () => {
            const next = change(read.result);
            if (next !== undefined) store.put(next, key);
          };
          transaction.oncomplete = () => resolve();
          transaction.onerror = () => reject(transaction.error);
          transaction.onabort = () => reject(transaction.error || new Error('aborted'));
        } catch (error) {
          reject(error);
        }
      }),
  };
}

/** The key range of one monolog: `[id]` … `[id, []]` holds every recorded string. */
function monologRange(monolog) {
  return IDBKeyRange.bound([Number(monolog)], [Number(monolog), []]);
}

function cursorRequest(store, range, direction) {
  return store.openCursor(range, direction);
}

/**
 * The message store: pages over `[monolog, recorded, id]`, oldest-first pages
 * in the answer. A page reads one row past `limit` so `hasMore` is known
 * without counting the whole store.
 */
export function messageStore(db) {
  return {
    /** Store a page of messages in one transaction. */
    async put(list = []) {
      if (!Array.isArray(list) || list.length === 0) return 0;
      return new Promise((resolve, reject) => {
        try {
          const transaction = db.transaction(MESSAGES, 'readwrite');
          const store = transaction.objectStore(MESSAGES);
          let stored = 0;
          for (const message of list) {
            const id = Number(message.id);
            const monolog = Number(message.monolog);
            if (!Number.isFinite(id) || !Number.isFinite(monolog)) continue;
            store.put({
              ...message,
              id,
              monolog,
              recorded: String(message.recorded ?? ''),
            });
            stored += 1;
          }
          transaction.oncomplete = () => resolve(stored);
          transaction.onerror = () => reject(transaction.error);
          transaction.onabort = () => reject(transaction.error || new Error('aborted'));
        } catch (error) {
          reject(error);
        }
      });
    },

    /**
     * One page, oldest-first.
     *
     * `before` asks for messages older than that cursor, `after` for newer
     * ones. With neither the newest page is returned. Both cursors are
     * exclusive, exactly like the server's.
     */
    async page(monolog, { before = null, after = null, limit = 50 } = {}) {
      const id = Number(monolog);
      const size = Math.max(1, Math.min(500, Number(limit) || 50));
      if (!Number.isFinite(id)) return { messages: [], hasMore: false };
      const upper = [id, []];
      let range;
      let direction;
      if (before) {
        range = IDBKeyRange.bound(
          [id],
          [id, String(before.recorded ?? ''), Number(before.id)],
          false,
          true,
        );
        direction = 'prev';
      } else if (after) {
        range = IDBKeyRange.bound(
          [id, String(after.recorded ?? ''), Number(after.id)],
          upper,
          true,
          false,
        );
        direction = 'next';
      } else {
        range = IDBKeyRange.bound([id], upper);
        direction = 'prev';
      }
      const found = await new Promise((resolve, reject) => {
        const collected = [];
        try {
          const transaction = db.transaction(MESSAGES, 'readonly');
          const store = transaction.objectStore(MESSAGES);
          const request = cursorRequest(store, range, direction);
          request.onerror = () => reject(request.error);
          request.onsuccess = () => {
            const cursor = request.result;
            if (!cursor) return;
            collected.push(cursor.value);
            // One extra row tells the caller whether another page exists.
            if (collected.length > size) return;
            cursor.continue();
          };
          transaction.oncomplete = () => resolve(collected);
          transaction.onerror = () => reject(transaction.error);
          transaction.onabort = () => reject(transaction.error || new Error('aborted'));
        } catch (error) {
          reject(error);
        }
      });
      const hasMore = found.length > size;
      const messages = found.slice(0, size);
      // `prev` walked backwards; the feed is always oldest-first.
      if (direction === 'prev') messages.reverse();
      return { messages, hasMore };
    },

    /** How many messages one monolog has on disk. */
    async count(monolog) {
      const id = Number(monolog);
      if (!Number.isFinite(id)) return 0;
      return new Promise((resolve, reject) => {
        try {
          const transaction = db.transaction(MESSAGES, 'readonly');
          const request = transaction.objectStore(MESSAGES).count(monologRange(id));
          request.onsuccess = () => resolve(Number(request.result) || 0);
          request.onerror = () => reject(request.error);
          transaction.onerror = () => reject(transaction.error);
          transaction.onabort = () => reject(transaction.error || new Error('aborted'));
        } catch (error) {
          reject(error);
        }
      });
    },

    /** Remove one message from disk. */
    async remove(monolog, recorded, id) {
      return new Promise((resolve, reject) => {
        try {
          const transaction = db.transaction(MESSAGES, 'readwrite');
          transaction
            .objectStore(MESSAGES)
            .delete(messageKey(monolog, recorded, id));
          transaction.oncomplete = () => resolve();
          transaction.onerror = () => reject(transaction.error);
          transaction.onabort = () => reject(transaction.error || new Error('aborted'));
        } catch (error) {
          reject(error);
        }
      });
    },

    /** Is there any message at all on this phone? */
    async any() {
      return new Promise((resolve, reject) => {
        try {
          const transaction = db.transaction(MESSAGES, 'readonly');
          const request = transaction.objectStore(MESSAGES).count();
          request.onsuccess = () => resolve(Number(request.result) > 0);
          request.onerror = () => reject(request.error);
          transaction.onerror = () => reject(transaction.error);
          transaction.onabort = () => reject(transaction.error || new Error('aborted'));
        } catch (error) {
          reject(error);
        }
      });
    },
  };
}

/** Every bag the page needs, from one connection. */
export async function openStorage(factory = indexedDB) {
  const db = await openDatabase(factory);
  return {
    db,
    store: storeBag(db, 'takes'),
    chunks: storeBag(db, 'audio'),
    monologs: storeBag(db, 'monologs'),
    messages: messageStore(db),
  };
}
