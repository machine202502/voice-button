// Tests for the phone's IndexedDB adapter.
//
//   vitest run src/data
//
// The browser is not here, so a small fake `indexedDB` is: it has the same
// shape the adapter uses (open → upgrade → transaction → store → get/put/keys,
// plus cursor and key range for the paged message store). It is written here,
// not imported, so the data layer keeps having no test dependency.
//
// The schema is deliberately a fresh database (`voice-button-v5`): there is no
// upgrade test, because an old `voice-button` database is never opened.

import { describe, expect, it } from 'vitest';

import { createCache } from './cache';
import { DB_NAME, LOCAL_KEY_PATH, MESSAGE_KEY_PATH, VERSION, openStorage } from './storage';

type FakeKey = number | string | FakeKey[];

interface FakeEntry {
  key: FakeKey;
  value: unknown;
}

interface FakeStore {
  name: string;
  keyPath: string | string[] | null;
  autoIncrement: boolean;
  entries: Map<string, FakeEntry>;
  next: number;
}

interface FakeRequest<T> {
  result: T | undefined;
  onsuccess: (() => void) | null;
  onerror: (() => void) | null;
}

interface FakeCursor {
  key: FakeKey;
  value: unknown;
  continue(): void;
}

interface FakeStoreApi {
  get(key: FakeKey): FakeRequest<unknown>;
  put(value: unknown, key?: FakeKey): FakeRequest<FakeKey | undefined>;
  delete(key: FakeKey): FakeRequest<undefined>;
  getAllKeys(range?: FakeRange): FakeRequest<FakeKey[]>;
  count(range?: FakeRange): FakeRequest<number>;
  openCursor(range: FakeRange | undefined, direction: 'next' | 'prev' | undefined): FakeRequest<FakeCursor | null>;
}

interface FakeTransaction {
  error: Error | null;
  completed: boolean;
  pending: number;
  oncomplete: (() => void) | null;
  onerror: (() => void) | null;
  onabort: (() => void) | null;
  objectStore(name?: string): FakeStoreApi;
}

interface FakeDatabase {
  name: string;
  version: number;
  stores: Map<string, FakeStore>;
  objectStoreNames: { contains(store: string): boolean };
  createObjectStore(
    store: string,
    options?: { keyPath?: string | string[]; autoIncrement?: boolean },
  ): FakeStore;
  deleteObjectStore(store: string): void;
  transaction(store: string): FakeTransaction;
}

interface FakeOpenRequest {
  result: FakeDatabase | null;
  error: Error | null;
  onupgradeneeded: ((event: { oldVersion: number; newVersion: number }) => void) | null;
  onsuccess: (() => void) | null;
  onerror: (() => void) | null;
}

// ---------------------------------------------------------------- key order

function rank(value: FakeKey): number {
  if (typeof value === 'number') return 0;
  if (typeof value === 'string') return 2;
  if (Array.isArray(value)) return 4;
  return 3;
}

function compareValues(left: FakeKey, right: FakeKey): number {
  if (Array.isArray(left) && Array.isArray(right)) {
    const length = Math.min(left.length, right.length);
    for (let index = 0; index < length; index += 1) {
      const a = left[index];
      const b = right[index];
      if (a === undefined || b === undefined) continue;
      const order = compareValues(a, b);
      if (order !== 0) return order;
    }
    return left.length - right.length;
  }
  const a = rank(left);
  const b = rank(right);
  if (a !== b) return a - b;
  if (typeof left === 'string' && typeof right === 'string') {
    return left < right ? -1 : left > right ? 1 : 0;
  }
  if (typeof left === 'number' && typeof right === 'number') return left - right;
  return 0;
}

class FakeRange {
  constructor(
    readonly lower: FakeKey | undefined,
    readonly upper: FakeKey | undefined,
    readonly lowerOpen: boolean,
    readonly upperOpen: boolean,
  ) {}

  static bound(lower: FakeKey, upper: FakeKey, lowerOpen = false, upperOpen = false): FakeRange {
    return new FakeRange(lower, upper, lowerOpen, upperOpen);
  }

  includes(key: FakeKey): boolean {
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

(globalThis as unknown as { IDBKeyRange: typeof FakeRange }).IDBKeyRange = FakeRange;

// ---------------------------------------------------------------- fake IDB

const keyId = (key: FakeKey): string => JSON.stringify(key);

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

function deriveKey(store: FakeStore, value: unknown): FakeKey | undefined {
  const keyPath = store.keyPath;
  if (!keyPath) return undefined;
  if (!isRecord(value)) return undefined;
  const partOf = (part: unknown): FakeKey | undefined =>
    typeof part === 'number' || typeof part === 'string' ? part : undefined;
  if (Array.isArray(keyPath)) {
    const parts: FakeKey[] = [];
    for (const field of keyPath) {
      parts.push(partOf(value[field]) ?? '');
    }
    return parts;
  }
  return partOf(value[keyPath]);
}

function fakeIndexedDB(): {
  databases: Map<string, FakeDatabase>;
  open(name: string, version: number): FakeOpenRequest;
} {
  const databases = new Map<string, FakeDatabase>();

  function makeStore(
    name: string,
    options: { keyPath?: string | string[]; autoIncrement?: boolean } = {},
  ): FakeStore {
    return {
      name,
      keyPath: options.keyPath ?? null,
      autoIncrement: options.autoIncrement === true,
      entries: new Map(),
      next: 1,
    };
  }

  // A transaction completes once every request it started has settled.
  function makeTransaction(store: FakeStore): FakeTransaction {
    const transaction: FakeTransaction = {
      error: null,
      completed: false,
      pending: 0,
      oncomplete: null,
      onerror: null,
      onabort: null,
      objectStore: () => api,
    };
    const maybeComplete = (): void => {
      if (transaction.pending > 0 || transaction.completed) return;
      transaction.completed = true;
      queueMicrotask(() => {
        if (transaction.oncomplete) transaction.oncomplete();
      });
    };
    const request = <T>(deliver: (handle: FakeRequest<T>) => void): FakeRequest<T> => {
      transaction.pending += 1;
      const handle: FakeRequest<T> = { result: undefined, onsuccess: null, onerror: null };
      queueMicrotask(() => {
        try {
          deliver(handle);
        } catch (error) {
          transaction.error = error instanceof Error ? error : new Error(String(error));
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

    const api: FakeStoreApi = {
      get(key) {
        return request<unknown>((handle) => {
          const found = store.entries.get(keyId(key));
          handle.result = found ? found.value : undefined;
        });
      },
      put(value, key) {
        return request<FakeKey | undefined>((handle) => {
          let actual: FakeKey | undefined;
          if (store.keyPath) {
            actual = deriveKey(store, value);
            if (actual === undefined && store.autoIncrement) {
              actual = store.next;
              store.next += 1;
            }
          } else {
            actual = key;
          }
          if (actual === undefined) throw new Error('a stored value needs a key');
          let stored = value;
          // IndexedDB writes an auto-generated key back into the record.
          const keyPath = store.keyPath;
          if (
            typeof keyPath === 'string' &&
            store.autoIncrement &&
            isRecord(value) &&
            value[keyPath] === undefined
          ) {
            stored = { ...value, [keyPath]: actual };
          }
          store.entries.set(keyId(actual), { key: actual, value: stored });
          handle.result = actual;
        });
      },
      delete(key) {
        return request<undefined>((handle) => {
          store.entries.delete(keyId(key));
          handle.result = undefined;
        });
      },
      getAllKeys(range) {
        return request<FakeKey[]>((handle) => {
          handle.result = [...store.entries.values()]
            .filter((entry) => !range || range.includes(entry.key))
            .map((entry) => entry.key)
            .sort(compareValues);
        });
      },
      count(range) {
        return request<number>((handle) => {
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
        const handle: FakeRequest<FakeCursor | null> = {
          result: undefined,
          onsuccess: null,
          onerror: null,
        };
        const advance = (): void => {
          transaction.pending += 1;
          queueMicrotask(() => {
            const entry = entries[index];
            if (!entry) {
              handle.result = null;
            } else {
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
    return transaction;
  }

  function makeDatabase(name: string, version: number): FakeDatabase {
    const stores = new Map<string, FakeStore>();
    return {
      name,
      version,
      stores,
      objectStoreNames: { contains: (store: string) => stores.has(store) },
      createObjectStore(store, options = {}) {
        const created = makeStore(store, options);
        stores.set(store, created);
        return created;
      },
      deleteObjectStore(store) {
        stores.delete(store);
      },
      transaction(store) {
        const found = stores.get(store);
        if (!found) throw new Error('no such store: ' + store);
        return makeTransaction(found);
      },
    };
  }

  function open(name: string, version: number): FakeOpenRequest {
    const handle: FakeOpenRequest = {
      result: null,
      error: null,
      onupgradeneeded: null,
      onsuccess: null,
      onerror: null,
    };
    queueMicrotask(() => {
      const existing = databases.get(name);
      const oldVersion = existing ? existing.version : 0;
      let database = existing;
      let upgrade = false;
      if (!database) {
        upgrade = true;
        database = makeDatabase(name, version);
        databases.set(name, database);
      } else if (version > database.version) {
        upgrade = true;
        database.version = version;
      }
      handle.result = database;
      if (upgrade && handle.onupgradeneeded) {
        handle.onupgradeneeded({ oldVersion, newVersion: version });
      }
      if (handle.onsuccess) handle.onsuccess();
    });
    return handle;
  }

  return { databases, open };
}

function openFake(): ReturnType<typeof fakeIndexedDB> {
  return fakeIndexedDB();
}

const factoryOf = (factory: ReturnType<typeof fakeIndexedDB>): IDBFactory =>
  factory as unknown as IDBFactory;

const day = (n: number): string =>
  '2026-11-' + String(10 + n).padStart(2, '0') + 'T10:00:00+05:00';

// ---------------------------------------------------------------- tests

describe('the storage adapter', () => {
  it('stores and returns values', async () => {
    const factory = openFake();
    const storage = await openStorage(factoryOf(factory));
    await storage.store.put('a', { localId: 'a', recorded: 'x', monolog: 1, remote: null, closed: false, seq: 0 });
    const read = await storage.store.get('a');
    expect(read?.localId).toBe('a');
    expect((await storage.store.keys()).join(',')).toBe('a');
    await storage.store.update('a', (previous) =>
      previous ? { ...previous, remote: 5 } : undefined,
    );
    const changed = await storage.store.get('a');
    expect(changed?.remote).toBe(5);
    expect(changed?.localId).toBe('a');
    await storage.store.remove('a');
    expect(await storage.store.get('a')).toBeUndefined();
    expect((await storage.store.keys()).length).toBe(0);
  });

  it('reuses the database when it is opened twice', async () => {
    const factory = openFake();
    await openStorage(factoryOf(factory));
    await openStorage(factoryOf(factory));
    expect(factory.databases.size).toBe(1);
  });

  it('opens the fresh v5 database with the offline stores', async () => {
    const factory = openFake();
    const storage = await openStorage(factoryOf(factory));
    const database = factory.databases.get(DB_NAME);
    expect(database).toBeDefined();
    expect(database?.version).toBe(VERSION);
    expect(VERSION).toBe(5);
    expect(DB_NAME).toBe('voice-button-v5');
    expect(LOCAL_KEY_PATH.join(',')).toBe('kind,id');
    expect(MESSAGE_KEY_PATH.join(',')).toBe('monolog,recorded,id');
    expect(database?.stores.get('local')?.keyPath).toEqual(['kind', 'id']);
    expect(database?.stores.get('mutations')?.keyPath).toBe('seq');
    expect(database?.stores.get('mutations')?.autoIncrement).toBe(true);
    expect(database?.stores.get('messages')?.keyPath).toEqual(['monolog', 'recorded', 'id']);
    expect(storage.local).toBeDefined();
    expect(storage.mutations).toBeDefined();
  });
});

describe('messages are one record per message, read in pages', () => {
  it('pages by the compound cursor and counts per monolog', async () => {
    const factory = openFake();
    const storage = await openStorage(factoryOf(factory));
    await storage.messages.put([
      { id: 1, monolog: 4, recorded: day(1), text: 'one' },
      { id: 2, monolog: 4, recorded: day(2), text: 'two' },
      { id: 3, monolog: 4, recorded: day(2), text: 'three' },
      { id: 4, monolog: 4, recorded: day(3), text: 'four' },
      { id: 5, monolog: 4, recorded: day(4), text: 'five' },
      { id: 9, monolog: 9, recorded: day(4), text: 'other' },
    ]);
    expect(await storage.messages.count(4)).toBe(5);
    expect((await storage.messages.ids(4)).join(',')).toBe('1,2,3,4,5');

    const latest = await storage.messages.page(4, { limit: 2 });
    expect(latest.messages.map((message) => message.id)).toEqual([4, 5]);
    expect(latest.hasMore).toBe(true);

    const older = await storage.messages.page(4, {
      before: { recorded: day(3), id: 4 },
      limit: 2,
    });
    expect(older.messages.map((message) => message.id)).toEqual([2, 3]);

    const newer = await storage.messages.page(4, {
      after: { recorded: day(2), id: 3 },
      limit: 2,
    });
    expect(newer.messages.map((message) => message.id)).toEqual([4, 5]);

    const found = await storage.messages.find(4, 3);
    expect(found?.text).toBe('three');
    expect(await storage.messages.find(4, 99)).toBeNull();

    await storage.messages.remove(4, day(2), 3);
    expect(await storage.messages.count(4)).toBe(4);
    expect(await storage.messages.any()).toBe(true);
  });
});

describe('the mutations queue store', () => {
  it('assigns seq itself and keeps the order', async () => {
    const factory = openFake();
    const storage = await openStorage(factoryOf(factory));
    const first = await storage.mutations.put({
      kind: 'monolog.rename',
      target: { monolog: 12 },
      payload: { name: 'Дом' },
      local: false,
      createdAt: '2026-11-13T10:00:00+05:00',
    });
    const second = await storage.mutations.put({
      kind: 'message.delete',
      target: { monolog: 12, message: 7 },
      payload: {},
      local: false,
      createdAt: '2026-11-13T10:01:00+05:00',
    });
    expect(first).toBe(1);
    expect(second).toBe(2);
    expect((await storage.mutations.keys()).map(Number)).toEqual([1, 2]);
    const stored = await storage.mutations.get(2);
    expect(stored?.kind).toBe('message.delete');
    // The store's key is written back into the record, like IndexedDB does.
    expect(stored && 'seq' in stored ? stored.seq : undefined).toBe(2);
    await storage.mutations.remove(1);
    expect((await storage.mutations.keys()).length).toBe(1);
  });
});

describe('local changes survive a reopen', () => {
  it('keeps an edit and a tombstone after the database is opened again', async () => {
    const factory = openFake();
    const recorded = day(1);

    const first = await openStorage(factoryOf(factory));
    const firstCache = createCache({
      monologs: first.monologs,
      messages: first.messages,
      local: first.local,
      now: () => '2026-11-13T10:00:00+05:00',
    });
    await firstCache.putMessages(4, [
      { id: 1, monolog: 4, recorded, text: 'server one' },
      { id: 2, monolog: 4, recorded: day(2), text: 'server two' },
    ]);
    await firstCache.applyEdit({ id: 1, monolog: 4, recorded }, 'edited on the phone');
    await firstCache.applyDelete({ id: 2, monolog: 4, recorded: day(2) });

    // A page reload opens the same database from scratch.
    const second = await openStorage(factoryOf(factory));
    const secondCache = createCache({
      monologs: second.monologs,
      messages: second.messages,
      local: second.local,
      now: () => '2026-11-13T11:00:00+05:00',
    });

    const page = await secondCache.messagePage(4, { limit: 10 });
    expect(page.messages.map((message) => message.id)).toEqual([1]);
    expect(page.messages[0]?.text).toBe('edited on the phone');
    expect(await secondCache.messageCount(4)).toBe(1);
    expect(await secondCache.message(4, 2)).toBeNull();
    expect((await secondCache.overrides()).map((override) => [override.kind, override.id])).toEqual([
      ['message', 1],
      ['message', 2],
    ]);

    // The tombstone hid the row; it did not delete it, so the next server page
    // cannot make the deletion undo itself.
    const stored = await second.messages.find(4, 2);
    expect(stored?.text).toBe('server two');

    await secondCache.clearOverride('message', 1);
    const restored = await secondCache.message(4, 1);
    expect(restored?.text).toBe('server one');
  });
});
