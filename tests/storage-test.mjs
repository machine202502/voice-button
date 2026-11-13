// Tests for the phone's IndexedDB adapter.
//
//   node tests/storage-test.mjs
//
// The browser is not here, so a small fake `indexedDB` is: it has the same
// shape the adapter uses (open → upgrade → transaction → store → get/put/keys,
// plus cursor and key range for the paged message store).

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

// ---------------------------------------------------------------- key order

function rank(value) {
  if (typeof value === 'number') return 0;
  if (value instanceof Date) return 1;
  if (typeof value === 'string') return 2;
  if (Array.isArray(value)) return 4;
  return 3;
}

function compareValues(left, right) {
  if (Array.isArray(left) && Array.isArray(right)) {
    const length = Math.min(left.length, right.length);
    for (let index = 0; index < length; index += 1) {
      const order = compareValues(left[index], right[index]);
      if (order !== 0) return order;
    }
    return left.length - right.length;
  }
  const a = rank(left);
  const b = rank(right);
  if (a !== b) return a - b;
  if (a === 2) return left < right ? -1 : left > right ? 1 : 0;
  return left - right;
}

class FakeRange {
  constructor(lower, upper, lowerOpen, upperOpen) {
    this.lower = lower;
    this.upper = upper;
    this.lowerOpen = lowerOpen;
    this.upperOpen = upperOpen;
  }

  static bound(lower, upper, lowerOpen = false, upperOpen = false) {
    return new FakeRange(lower, upper, lowerOpen, upperOpen);
  }

  includes(key) {
    if (this.lower !== undefined) {
      const order = compareValues(key, this.lower);
      if (order < 0 || (order === 0 && this.lowerOpen)) return false;
    }
    if (this.upper !== undefined) {
      const order = compareValues(key, this.upper);
      if (order > 0 || (order === 0 && this.upperOpen)) return false;
    }
    return true;
  }
}

globalThis.IDBKeyRange = FakeRange;

// ---------------------------------------------------------------- fake IDB

const keyId = (key) => JSON.stringify(key);

function deriveKey(store, value) {
  if (!store.keyPath) return undefined;
  if (Array.isArray(store.keyPath)) {
    return store.keyPath.map((field) => value[field]);
  }
  return value[store.keyPath];
}

function fakeIndexedDB() {
  const databases = new Map();

  function makeStore(name, keyPath) {
    return { name, keyPath: keyPath ?? null, entries: new Map() };
  }

  // A transaction completes once every request it started has settled.
  function makeTransaction(store) {
    const transaction = {
      error: null,
      completed: false,
      pending: 0,
      oncomplete: null,
      onerror: null,
      onabort: null,
    };
    const maybeComplete = () => {
      if (transaction.pending > 0 || transaction.completed) return;
      transaction.completed = true;
      queueMicrotask(() => {
        if (transaction.oncomplete) transaction.oncomplete();
      });
    };
    const request = (deliver) => {
      transaction.pending += 1;
      const handle = { result: undefined, onsuccess: null, onerror: null };
      queueMicrotask(() => {
        try {
          deliver(handle);
        } catch (error) {
          transaction.error = error;
          transaction.pending -= 1;
          if (transaction.onerror) transaction.onerror();
          return;
        }
        if (handle.onsuccess) handle.onsuccess();
        transaction.pending -= 1;
        maybeComplete();
      });
      return handle;
    };

    const api = {
      get(key) {
        return request((handle) => {
          const found = store.entries.get(keyId(key));
          handle.result = found ? found.value : undefined;
        });
      },
      put(value, key) {
        const actual = store.keyPath ? deriveKey(store, value) : key;
        return request((handle) => {
          store.entries.set(keyId(actual), { key: actual, value });
          handle.result = actual;
        });
      },
      delete(key) {
        return request((handle) => {
          store.entries.delete(keyId(key));
          handle.result = undefined;
        });
      },
      getAllKeys() {
        return request((handle) => {
          handle.result = [...store.entries.values()]
            .map((entry) => entry.key)
            .sort(compareValues);
        });
      },
      count(range) {
        return request((handle) => {
          let count = 0;
          for (const entry of store.entries.values()) {
            if (!range || range.includes(entry.key)) count += 1;
          }
          handle.result = count;
        });
      },
      openCursor(range, direction) {
        const entries = [...store.entries.values()]
          .filter((entry) => !range || range.includes(entry.key))
          .sort((left, right) => compareValues(left.key, right.key));
        if (direction === 'prev') entries.reverse();
        let index = 0;
        const handle = {
          result: undefined,
          onsuccess: null,
          onerror: null,
        };
        const advance = () => {
          transaction.pending += 1;
          queueMicrotask(() => {
            if (index >= entries.length) {
              handle.result = null;
            } else {
              const entry = entries[index];
              handle.result = {
                key: entry.key,
                value: entry.value,
                continue() {
                  index += 1;
                  advance();
                },
              };
            }
            if (handle.onsuccess) handle.onsuccess();
            transaction.pending -= 1;
            maybeComplete();
          });
        };
        advance();
        return handle;
      },
    };
    transaction.objectStore = () => api;
    transaction._maybeComplete = maybeComplete;
    return transaction;
  }

  function open(name, version) {
    const handle = {
      result: null,
      error: null,
      onupgradeneeded: null,
      onsuccess: null,
      onerror: null,
    };
    queueMicrotask(() => {
      let database = databases.get(name);
      let upgrade = false;
      if (!database) {
        upgrade = true;
        database = {
          name,
          version,
          stores: new Map(),
          objectStoreNames: { contains: (store) => database.stores.has(store) },
          createObjectStore(store, options = {}) {
            database.stores.set(store, makeStore(store, options.keyPath));
            return {};
          },
          deleteObjectStore(store) {
            database.stores.delete(store);
          },
          transaction(store) {
            const found = database.stores.get(store);
            if (!found) throw new Error('no such store: ' + store);
            return makeTransaction(found);
          },
        };
        databases.set(name, database);
      } else if (version > database.version) {
        upgrade = true;
        database.version = version;
      }
      handle.result = database;
      if (upgrade && handle.onupgradeneeded) handle.onupgradeneeded({});
      if (handle.onsuccess) handle.onsuccess();
    });
    return handle;
  }

  return { databases, open };
}

// ---------------------------------------------------------------- tests

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

console.log('messages are one record per message, read in pages');
{
  const factory = fakeIndexedDB();
  const storage = await openStorage(factory);
  const day = (n) => '2026-11-' + String(10 + n).padStart(2, '0') + 'T10:00:00+05:00';
  await storage.messages.put([
    { id: 1, monolog: 4, recorded: day(1), text: 'one' },
    { id: 2, monolog: 4, recorded: day(2), text: 'two' },
    { id: 3, monolog: 4, recorded: day(2), text: 'three' },
    { id: 4, monolog: 4, recorded: day(3), text: 'four' },
    { id: 5, monolog: 4, recorded: day(4), text: 'five' },
    { id: 9, monolog: 9, recorded: day(4), text: 'other' },
  ]);
  check((await storage.messages.count(4)) === 5, 'count is per monolog');

  const latest = await storage.messages.page(4, { limit: 2 });
  check(
    latest.messages.map((message) => message.id).join(',') === '4,5',
    'the newest page is oldest-first: ' + latest.messages.map((message) => message.id),
  );
  check(latest.hasMore === true, 'one extra row says there is another page');

  const older = await storage.messages.page(4, {
    before: { recorded: day(3), id: 4 },
    limit: 2,
  });
  check(
    older.messages.map((message) => message.id).join(',') === '2,3',
    'before is exclusive and returns the nearest older rows: ' + older.messages.map((m) => m.id),
  );

  const newer = await storage.messages.page(4, {
    after: { recorded: day(2), id: 3 },
    limit: 2,
  });
  check(
    newer.messages.map((message) => message.id).join(',') === '4,5',
    'after is exclusive and returns newer rows: ' + newer.messages.map((m) => m.id),
  );

  await storage.messages.remove(4, day(2), 3);
  check((await storage.messages.count(4)) === 4, 'deleting one message leaves the rest');
  check((await storage.messages.any()) === true, 'any() sees the other monolog too');
}

console.log('the version bump drops the old messages store');
{
  const factory = fakeIndexedDB();
  // A phone that still has the old shape: one array per monolog.
  const old = factory.open('voice-button', 3);
  await new Promise((resolve) => {
    old.onupgradeneeded = () => {
      const db = old.result;
      for (const name of ['takes', 'audio', 'outbox', 'chunks', 'monologs']) {
        db.createObjectStore(name);
      }
      db.createObjectStore('messages');
    };
    old.onsuccess = resolve;
  });
  const oldDb = old.result;
  await new Promise((resolve, reject) => {
    const transaction = oldDb.transaction('messages', 'readwrite');
    transaction.objectStore('messages').put([{ id: 1, text: 'old' }], 1);
    transaction.oncomplete = resolve;
    transaction.onerror = () => reject(transaction.error);
  });

  const storage = await openStorage(factory);
  const database = factory.databases.get('voice-button');
  check(database.version === 4, 'the database is at version 4');
  check(
    Array.isArray(database.stores.get('messages').keyPath) &&
      database.stores.get('messages').keyPath.join(',') === 'monolog,recorded,id',
    'messages have the compound key path',
  );
  const page = await storage.messages.page(1, { limit: 10 });
  check(page.messages.length === 0, 'the old array is gone, not migrated');
}

console.log('');
console.log(`${checks - failures}/${checks} checks passed`);
process.exit(failures === 0 ? 0 : 1);
