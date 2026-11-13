// Tests for the phone's IndexedDB adapter.
//
//   node tests/storage-test.mjs
//
// The browser is not here, so a small fake `indexedDB` is: it has the same
// shape the adapter uses (open → transaction → store → get/put/delete/keys) and
// answers on the next microtask, like the real one answers later.

import { openStorage } from '../src/web/js/storage.mjs';
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

/** Just enough IndexedDB for the adapter, with no browser. */
function fakeIndexedDB() {
  const databases = new Map();
  return {
    databases,
    open(name, version) {
      const request = { result: null, error: null };
      let database = databases.get(name);
      const created = !database;
      if (!database) {
        database = {
          name,
          version,
          stores: new Map(),
          objectStoreNames: { contains: (store) => database.stores.has(store) },
          createObjectStore(store) {
            database.stores.set(store, new Map());
            return {};
          },
          transaction(store, mode) {
            const data = database.stores.get(store);
            if (!data) throw new Error('no such store: ' + store);
            const transaction = { error: null, objectStore: () => api };
            const settle = (value) => {
              const pending = Promise.resolve(value);
              pending.then(() => {
                if (transaction.oncomplete) transaction.oncomplete();
              });
              return { result: undefined, get result() { return undefined; }, ok: true };
            };
            const api = {
              get: (key) => queueResult(data.get(key), transaction),
              put: (value, key) => {
                data.set(key, value);
                return queueResult(undefined, transaction);
              },
              delete: (key) => {
                data.delete(key);
                return queueResult(undefined, transaction);
              },
              getAllKeys: () => queueResult([...data.keys()], transaction),
            };
            // The adapter's `update` reads and writes inside one transaction:
            // the fake does the same, so a lost read-modify-write would show.
            api.update = (key, change) => {
              const read = queueResult(data.get(key), transaction);
              read.onsuccess = () => {
                const next = change(read.result);
                if (next !== undefined) data.set(key, next);
              };
              return read;
            };
            void mode;
            void settle;
            return transaction;
          },
        };
        databases.set(name, database);
      }
      request.result = database;
      queueMicrotask(() => {
        if (created && request.onupgradeneeded) request.onupgradeneeded();
        if (request.onsuccess) request.onsuccess();
      });
      return request;
    },
  };
}

/** A request whose result arrives after the transaction completes. */
function queueResult(value, transaction) {
  const request = { result: undefined };
  queueMicrotask(() => {
    request.result = value;
    if (request.onsuccess) request.onsuccess();
    queueMicrotask(() => {
      if (transaction.oncomplete) transaction.oncomplete();
    });
  });
  return request;
}

console.log('the adapter stores and returns values');
{
  const factory = fakeIndexedDB();
  const storage = await openStorage(factory);
  await storage.store.put('a', { localId: 'a', recorded: 'x' });
  const read = await storage.store.get('a');
  check(read && read.localId === 'a', 'a value comes back as it went in');
  check((await storage.store.keys()).join(',') === 'a', 'keys lists what is there');
  await storage.store.update('a', (previous) => ({ ...previous, remote: 5 }));
  const changed = await storage.store.get('a');
  check(
    changed && changed.remote === 5 && changed.localId === 'a',
    'an update sees the value and keeps the rest',
  );
  await storage.store.remove('a');
  check((await storage.store.get('a')) === undefined, 'a removed value is gone');
  check((await storage.store.keys()).length === 0, 'and its key with it');
}

console.log('the outbox runs on it unchanged');
{
  const factory = fakeIndexedDB();
  const storage = await openStorage(factory);
  const outbox = createOutbox({
    store: storage.store,
    chunks: storage.chunks,
    now: () => '2026-11-12T15:00:00+05:00',
  });
  const take = outbox.newTake(1);
  await outbox.put(take);
  await outbox.putChunk(take.localId, 0, new Uint8Array([1]));
  await outbox.putChunk(take.localId, 1, new Uint8Array([2]));
  check((await outbox.list()).length === 1, 'the take is listed');
  check((await outbox.chunks(take.localId)).length === 2, 'its audio is there');
  await outbox.remove(take.localId);
  check((await outbox.list()).length === 0, 'removing the take removes it');
  check((await outbox.chunks(take.localId)).length === 0, 'and its audio with it');
}

console.log('opening twice reuses the database');
{
  const factory = fakeIndexedDB();
  await openStorage(factory);
  await openStorage(factory);
  check(factory.databases.size === 1, 'one database, not two');
}

console.log('the cache bags come from the same connection');
{
  const factory = fakeIndexedDB();
  const storage = await openStorage(factory);
  await storage.monologs.put(-1, { id: -1, localId: -1, name: 'Ждёт', serverId: null });
  await storage.messages.put(-1, [{ id: 1, text: 'старое' }]);
  const monolog = await storage.monologs.get(-1);
  const messages = await storage.messages.get(-1);
  check(monolog && monolog.name === 'Ждёт', 'a local monolog is stored by its negative id');
  check(
    Array.isArray(messages) && messages[0].text === 'старое',
    'messages are stored by the monolog id',
  );
  check(
    Boolean(storage.store && storage.chunks),
    'the takes and audio bags are still there for the outbox',
  );
}

console.log('');
console.log(`${checks - failures}/${checks} checks passed`);
process.exit(failures === 0 ? 0 : 1);
