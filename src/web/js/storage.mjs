// The phone's storage: IndexedDB behind the little bag interface the outbox
// expects (`get`, `put`, `remove`, `keys`).
//
// Kept separate from the outbox so the outbox itself is testable with a map and
// runs unchanged on the phone. Nothing here decides anything: it only moves
// values in and out.

const DB_NAME = 'voice-button';
const VERSION = 3;

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
      // What the page can still show without the computer: the last list of
      // monologs (negative keys for ones made here) and the last messages of
      // each monolog, keyed by its id.
      if (!db.objectStoreNames.contains('monologs')) db.createObjectStore('monologs');
      if (!db.objectStoreNames.contains('messages')) db.createObjectStore('messages');
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

/** Every bag the page needs, from one connection. */
export async function openStorage(factory = indexedDB) {
  const db = await openDatabase(factory);
  return {
    db,
    store: storeBag(db, 'takes'),
    chunks: storeBag(db, 'audio'),
    monologs: storeBag(db, 'monologs'),
    messages: storeBag(db, 'messages'),
  };
}
