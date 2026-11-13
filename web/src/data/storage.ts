// The phone's storage: IndexedDB behind the little bag interface the outbox
// expects (`get`, `put`, `remove`, `keys`, `update`) and a paged store for
// messages.
//
// Nothing here decides anything: it only moves values in and out. The schema is
// deliberately a fresh database: the page being replaced kept a `voice-button`
// database with a different shape, and that file is simply not opened any more.
// `voice-button-v5` is created from nothing on the first run.
//
// Messages are one record per message, keyed by
// `['monolog', 'recorded', 'id']`. That key is the cursor the API uses, and it
// is the order the feed is read in: a page is a key range, never a whole array.
//
// Two stores are added for offline changes:
//
//   * `local` — one record per row the person changed, keyed by
//     `['kind', 'id']`, where `kind` is `'monolog'` or `'message'`. Only the
//     fields that changed are present. Reads lay this over the server row;
//     nothing here deletes a cached row.
//   * `mutations` — the ordered queue of changes to send, keyed by `seq`, which
//     the store assigns itself (`autoIncrement`). This module only creates the
//     store and hands out a bag; the queue owns what goes in it.

import type {
  EntityId,
  Message,
  MessageInput,
  Monolog,
  MutationRecord,
  Override,
  Take,
} from './types';

// The queue's record lives in `types.ts`; the storage layer is where the rest
// of the data layer looks for the store contract, so it is re-exported here.
export type { MutationRecord } from './types';

export const DB_NAME = 'voice-button-v5';
export const VERSION = 5;
export const MESSAGE_KEY_PATH: string[] = ['monolog', 'recorded', 'id'];
export const LOCAL_KEY_PATH: string[] = ['kind', 'id'];

const MESSAGES = 'messages';
const LOCAL = 'local';
const MUTATIONS = 'mutations';

/**
 * One bag over a key/value object store. `put` takes either an explicit key
 * (stores without a key path) or only the value (key-path and auto-increment
 * stores), and answers with the key the store used.
 */
export interface StoreBag<T> {
  get(key: IDBValidKey): Promise<T | undefined>;
  put(value: T): Promise<IDBValidKey>;
  put(key: IDBValidKey, value: T): Promise<IDBValidKey>;
  remove(key: IDBValidKey): Promise<void>;
  keys(): Promise<IDBValidKey[]>;
  /**
   * Change one value from what is stored right now, in one transaction.
   *
   * A plain get followed by a put is two transactions: another writer can slip
   * between them and its field is lost.
   */
  update(key: IDBValidKey, change: (previous: T | undefined) => T | undefined): Promise<void>;
}

/** The `mutations` queue's store: `seq` is assigned by the store, not the caller. */
export interface MutationStore {
  /** Append one record and answer with the `seq` the store gave it. */
  put(value: MutationRecord): Promise<number>;
  get(seq: number): Promise<MutationRecord | undefined>;
  remove(seq: number): Promise<void>;
  keys(): Promise<unknown[]>;
}

/** One page of messages, oldest-first; `hasMore` reads one row past the page. */
export interface StoredMessagePage {
  messages: Message[];
  hasMore: boolean;
}

/** Where a page starts or ends; both cursors are exclusive, like the API's. */
export interface MessagePageOptions {
  before?: { id: EntityId; recorded?: string } | null;
  after?: { id: EntityId; recorded?: string } | null;
  limit?: number;
}

/** The message table: pages over `[monolog, recorded, id]`, oldest-first. */
export interface MessageStore {
  /** Store a page of messages in one transaction; returns how many were stored. */
  put(list?: readonly MessageInput[]): Promise<number>;
  /** One page, oldest-first, `before`/`after` exclusive like the server's. */
  page(monolog: number, options?: MessagePageOptions): Promise<StoredMessagePage>;
  /** How many messages one monolog has on disk. */
  count(monolog: number): Promise<number>;
  /** The ids of every message of one monolog on disk. */
  ids(monolog: number): Promise<number[]>;
  /** One message of one monolog, whatever its `recorded` stamp. */
  find(monolog: number, id: number): Promise<Message | null>;
  /** Remove one message from disk. */
  remove(monolog: number, recorded: string, id: EntityId): Promise<void>;
  /** Is there any message at all on this phone? */
  any(): Promise<boolean>;
}

/** Every bag the page needs, from one connection. */
export interface Storage {
  db: IDBDatabase;
  store: StoreBag<Take>;
  chunks: StoreBag<Uint8Array>;
  monologs: StoreBag<Monolog>;
  messages: MessageStore;
  local: StoreBag<Override>;
  mutations: MutationStore;
}

/** The key of one stored message; the same `recorded|id` order the API uses. */
export function messageKey(monolog: number, recorded: string, id: EntityId): IDBValidKey {
  return [Number(monolog), String(recorded), Number(id)];
}

/** Open (or create) the database. The stores are created in one upgrade. */
export function openDatabase(factory: IDBFactory = indexedDB): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = factory.open(DB_NAME, VERSION);
    request.onupgradeneeded = () => {
      const db = request.result;
      // The recording outbox and the takes it owns.
      db.createObjectStore('takes');
      db.createObjectStore('audio');
      // The monolog list: negative keys for the ones made here.
      db.createObjectStore('monologs');
      // The feed, read in pages. An older page kept one array per monolog;
      // that shape is gone with the old database and is not recreated.
      db.createObjectStore(MESSAGES, { keyPath: MESSAGE_KEY_PATH });
      // Local overrides: one record per changed row, merged by `[kind, id]`.
      db.createObjectStore(LOCAL, { keyPath: LOCAL_KEY_PATH });
      // The ordered queue of changes; the store assigns `seq` itself.
      db.createObjectStore(MUTATIONS, { keyPath: 'seq', autoIncrement: true });
    };
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(request.error);
  });
}

/** Run one request in its own transaction and resolve with its result. */
function requestOf<R>(
  db: IDBDatabase,
  name: string,
  mode: IDBTransactionMode,
  action: (store: IDBObjectStore) => IDBRequest<R>,
): Promise<R> {
  return new Promise((resolve, reject) => {
    let request: IDBRequest<R>;
    try {
      const transaction = db.transaction(name, mode);
      request = action(transaction.objectStore(name));
      transaction.oncomplete = () => resolve(request.result);
      transaction.onerror = () => reject(transaction.error);
      transaction.onabort = () => reject(transaction.error ?? new Error('aborted'));
    } catch (error) {
      reject(error);
    }
  });
}

/** One object store as a bag. */
export function storeBag<T>(db: IDBDatabase, name: string): StoreBag<T> {
  const run = <R>(
    mode: IDBTransactionMode,
    action: (store: IDBObjectStore) => IDBRequest<R>,
  ): Promise<R> => requestOf(db, name, mode, action);

  function put(value: T): Promise<IDBValidKey>;
  function put(key: IDBValidKey, value: T): Promise<IDBValidKey>;
  function put(...args: [T] | [IDBValidKey, T]): Promise<IDBValidKey> {
    const key = args.length === 2 ? (args[0] as IDBValidKey) : undefined;
    const value = (args.length === 2 ? args[1] : args[0]) as T;
    return run<IDBValidKey>('readwrite', (store) =>
      key === undefined ? store.put(value) : store.put(value, key),
    );
  }

  return {
    get: (key) => run<T | undefined>('readonly', (store) => store.get(key)),
    put,
    remove: async (key) => {
      await run<undefined>('readwrite', (store) => store.delete(key));
    },
    keys: async () => {
      const keys = await run<IDBValidKey[]>('readonly', (store) => store.getAllKeys());
      return Array.isArray(keys) ? keys : [];
    },
    update: (key, change) =>
      new Promise((resolve, reject) => {
        try {
          const transaction = db.transaction(name, 'readwrite');
          const store = transaction.objectStore(name);
          const read = store.get(key);
          read.onsuccess = () => {
            const next = change(read.result as T | undefined);
            if (next !== undefined) store.put(next, key);
          };
          transaction.oncomplete = () => resolve();
          transaction.onerror = () => reject(transaction.error);
          transaction.onabort = () => reject(transaction.error ?? new Error('aborted'));
        } catch (error) {
          reject(error);
        }
      }),
  };
}

/** The key range of one monolog: `[id]` … `[id, []]` holds every recorded string. */
function monologRange(monolog: number): IDBKeyRange {
  return IDBKeyRange.bound([monolog], [monolog, []]);
}

/** The auto-increment queue store: `put(record)` appends and answers with `seq`. */
function mutationStore(db: IDBDatabase): MutationStore {
  const run = <R>(
    mode: IDBTransactionMode,
    action: (store: IDBObjectStore) => IDBRequest<R>,
  ): Promise<R> => requestOf(db, MUTATIONS, mode, action);

  return {
    async put(value) {
      const key = await run<IDBValidKey>('readwrite', (store) => store.put(value));
      const seq = Number(key);
      if (!Number.isFinite(seq)) {
        throw new Error('the mutations store did not give the mutation a seq');
      }
      return seq;
    },
    get: (seq) => run<MutationRecord | undefined>('readonly', (store) => store.get(seq)),
    async remove(seq) {
      await run<undefined>('readwrite', (store) => store.delete(seq));
    },
    async keys() {
      return run<IDBValidKey[]>('readonly', (store) => store.getAllKeys());
    },
  };
}

/** The message store: pages over `[monolog, recorded, id]`, oldest-first. */
export function messageStore(db: IDBDatabase): MessageStore {
  return {
    /** Store a page of messages in one transaction. */
    async put(list: readonly MessageInput[] = []) {
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
          transaction.onabort = () => reject(transaction.error ?? new Error('aborted'));
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
      const upper: IDBValidKey = [id, []];
      let range: IDBKeyRange;
      let direction: IDBCursorDirection;
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
      const found = await new Promise<Message[]>((resolve, reject) => {
        const collected: Message[] = [];
        try {
          const transaction = db.transaction(MESSAGES, 'readonly');
          const store = transaction.objectStore(MESSAGES);
          const request = store.openCursor(range, direction);
          request.onerror = () => reject(request.error);
          request.onsuccess = () => {
            const cursor = request.result;
            if (!cursor) return;
            collected.push(cursor.value as Message);
            // One extra row tells the caller whether another page exists.
            if (collected.length > size) return;
            cursor.continue();
          };
          transaction.oncomplete = () => resolve(collected);
          transaction.onerror = () => reject(transaction.error);
          transaction.onabort = () => reject(transaction.error ?? new Error('aborted'));
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
          transaction.onabort = () => reject(transaction.error ?? new Error('aborted'));
        } catch (error) {
          reject(error);
        }
      });
    },

    /** The ids of every message of one monolog on disk. */
    async ids(monolog) {
      const id = Number(monolog);
      if (!Number.isFinite(id)) return [];
      const keys = await new Promise<IDBValidKey[]>((resolve, reject) => {
        try {
          const transaction = db.transaction(MESSAGES, 'readonly');
          const request = transaction.objectStore(MESSAGES).getAllKeys(monologRange(id));
          request.onsuccess = () => resolve(request.result);
          request.onerror = () => reject(request.error);
          transaction.onerror = () => reject(transaction.error);
          transaction.onabort = () => reject(transaction.error ?? new Error('aborted'));
        } catch (error) {
          reject(error);
        }
      });
      const found: number[] = [];
      for (const key of keys) {
        if (!Array.isArray(key)) continue;
        const messageId = Number(key[2]);
        if (Number.isFinite(messageId)) found.push(messageId);
      }
      return found;
    },

    /** One message of one monolog, whatever its `recorded` stamp. */
    async find(monolog, messageId) {
      const id = Number(monolog);
      if (!Number.isFinite(id)) return null;
      return new Promise((resolve, reject) => {
        try {
          const transaction = db.transaction(MESSAGES, 'readonly');
          const store = transaction.objectStore(MESSAGES);
          const request = store.openCursor(monologRange(id), 'next');
          let found: Message | null = null;
          request.onerror = () => reject(request.error);
          request.onsuccess = () => {
            const cursor = request.result;
            if (!cursor || found) return;
            const record = cursor.value as Message;
            if (Number(record.id) === Number(messageId)) {
              found = record;
              return;
            }
            cursor.continue();
          };
          transaction.oncomplete = () => resolve(found);
          transaction.onerror = () => reject(transaction.error);
          transaction.onabort = () => reject(transaction.error ?? new Error('aborted'));
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
          transaction.objectStore(MESSAGES).delete(messageKey(monolog, recorded, id));
          transaction.oncomplete = () => resolve();
          transaction.onerror = () => reject(transaction.error);
          transaction.onabort = () => reject(transaction.error ?? new Error('aborted'));
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
          transaction.onabort = () => reject(transaction.error ?? new Error('aborted'));
        } catch (error) {
          reject(error);
        }
      });
    },
  };
}

/** Every bag the page needs, from one connection. */
export async function openStorage(factory: IDBFactory = indexedDB): Promise<Storage> {
  const db = await openDatabase(factory);
  return {
    db,
    store: storeBag<Take>(db, 'takes'),
    chunks: storeBag<Uint8Array>(db, 'audio'),
    monologs: storeBag<Monolog>(db, 'monologs'),
    messages: messageStore(db),
    local: storeBag<Override>(db, LOCAL),
    mutations: mutationStore(db),
  };
}
