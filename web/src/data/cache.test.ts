// Tests for the phone's offline cache and its pure decisions.
//
//   vitest run src/data
//
// No browser: the bags are maps with the same shape storage.ts hands the page.
// The cases are the ones a dead connection used to turn into a dead end: which
// monologs the list shows, how a local id is chosen, how boot decides, and the
// four manual changes that must survive a reload:
//
//   * an edit changes the text every read returns, and clearing it restores the
//     server's value;
//   * a delete hides the row from pages, `message` and the count, while the
//     cached row stays on disk;
//   * an archive moves a monolog between the two lists;
//   * a rename is visible before the computer answers.

import { describe, expect, it } from 'vitest';

import { bootOutcome, createCache, mergeMonologs, nextLocalId } from './cache';
import type { Cache } from './cache';
import type { Message, Monolog, Override } from './types';
import type { MessageStore, StoreBag, StoredMessagePage } from './storage';
import type { SyncCache } from './sync';

/** One key/value bag with the interface storage.ts exposes. */
interface FakeBag<T> extends StoreBag<T> {
  rows: Map<string, { key: IDBValidKey; value: T }>;
}

function bag<T>(derive?: (value: T) => IDBValidKey): FakeBag<T> {
  const rows = new Map<string, { key: IDBValidKey; value: T }>();
  const keyId = (key: IDBValidKey): string => JSON.stringify(key);

  function put(value: T): Promise<IDBValidKey>;
  function put(key: IDBValidKey, value: T): Promise<IDBValidKey>;
  async function put(first: T | IDBValidKey, second?: T): Promise<IDBValidKey> {
    const two = second !== undefined;
    const value = (two ? second : first) as T;
    const key = two ? (first as IDBValidKey) : derive ? derive(value) : undefined;
    if (key === undefined) throw new Error('the bag needs a key');
    rows.set(keyId(key), { key, value });
    return key;
  }

  return {
    rows,
    async get(key) {
      return rows.get(keyId(key))?.value;
    },
    put,
    async remove(key) {
      rows.delete(keyId(key));
    },
    async keys() {
      return [...rows.values()].map((row) => row.key);
    },
    async update(key, change) {
      const id = keyId(key);
      const current = rows.get(id);
      const next = change(current?.value);
      if (next !== undefined) rows.set(id, { key, value: next });
    },
  };
}

function localBag(): FakeBag<Override> {
  return bag<Override>((override) => [override.kind, override.id]);
}

function compareMessages(left: Message, right: Message): number {
  if (left.recorded < right.recorded) return -1;
  if (left.recorded > right.recorded) return 1;
  return left.id - right.id;
}

/** The paged message store, with the same contract storage.ts implements. */
interface FakeMessageStore extends MessageStore {
  rows: Map<string, Message>;
}

function messageStore(): FakeMessageStore {
  const rows = new Map<string, Message>();
  const key = (message: Message): string =>
    message.monolog + '|' + message.recorded + '|' + message.id;
  return {
    rows,
    async put(list = []) {
      let stored = 0;
      for (const input of list) {
        const id = Number(input.id);
        const monolog = Number(input.monolog);
        if (!Number.isFinite(id) || !Number.isFinite(monolog)) continue;
        const message: Message = {
          ...input,
          id,
          monolog,
          recorded: String(input.recorded ?? ''),
        };
        rows.set(key(message), message);
        stored += 1;
      }
      return stored;
    },
    async page(monolog, { before = null, after = null, limit = 50 } = {}): Promise<StoredMessagePage> {
      const all = [...rows.values()]
        .filter((message) => message.monolog === monolog)
        .sort(compareMessages);
      if (before) {
        const recorded = String(before.recorded ?? '');
        const id = Number(before.id);
        const older = all.filter(
          (message) => message.recorded < recorded || (message.recorded === recorded && message.id < id),
        );
        const chunk = older.slice(-(limit + 1));
        return { messages: chunk.slice(-limit), hasMore: chunk.length > limit };
      }
      if (after) {
        const recorded = String(after.recorded ?? '');
        const id = Number(after.id);
        const newer = all.filter(
          (message) => message.recorded > recorded || (message.recorded === recorded && message.id > id),
        );
        return { messages: newer.slice(0, limit), hasMore: newer.length > limit };
      }
      const chunk = all.slice(-(limit + 1));
      return { messages: chunk.slice(-limit), hasMore: chunk.length > limit };
    },
    async count(monolog) {
      return [...rows.values()].filter((message) => message.monolog === monolog).length;
    },
    async ids(monolog) {
      return [...rows.values()]
        .filter((message) => message.monolog === monolog)
        .map((message) => message.id);
    },
    async find(monolog, id) {
      for (const message of rows.values()) {
        if (message.monolog === monolog && message.id === id) return message;
      }
      return null;
    },
    async remove(monolog, recorded, id) {
      rows.delete(monolog + '|' + recorded + '|' + id);
    },
    async removeMonolog(monolog) {
      for (const [key, message] of rows) {
        if (message.monolog === monolog) rows.delete(key);
      }
    },
    async any() {
      return rows.size > 0;
    },
  };
}

function cacheWith(
  monologs: StoreBag<Monolog> = bag<Monolog>(),
  messages: MessageStore = messageStore(),
  local: StoreBag<Override> = localBag(),
): Cache {
  return createCache({
    monologs,
    messages,
    local,
    now: () => '2026-11-12T15:00:00+05:00',
  });
}

const recorded = '2026-11-12T10:00:00+05:00';
const later = '2026-11-12T11:00:00+05:00';

describe('local ids', () => {
  it('are negative and go down', () => {
    expect(nextLocalId([])).toBe(-1);
    expect(nextLocalId([-1])).toBe(-2);
    expect(nextLocalId([4, -1, 'x', -3])).toBe(-4);
    expect(nextLocalId(['a', 'b'])).toBe(-1);
  });
});

describe('the monolog list', () => {
  it('is cached monologs plus unsent local ones', () => {
    const server = [
      { id: 7, name: 'Работа', count: 3 },
      { id: 5, name: 'Дом', count: 1 },
    ];
    const local = [{ id: -1, localId: -1, name: 'В дороге', serverId: null }];
    const merged = mergeMonologs(server, local);
    expect(merged.map((item) => item.id)).toEqual([7, 5, -1]);
    expect(merged[2]?.local).toBe(true);
    expect(merged[0]?.local).toBe(false);
  });

  it('does not show a mapped monolog twice', () => {
    const server = [{ id: 9, name: 'Работа', count: 2 }];
    const mapped = [{ id: -1, localId: -1, name: 'Работа', serverId: 9 }];
    expect(mergeMonologs(server, mapped).length).toBe(1);
    const notListed = mergeMonologs(server, [
      { id: -2, localId: -2, name: 'Новый', serverId: 11 },
    ]);
    expect(notListed.map((item) => item.id)).toEqual([11, 9]);
    expect(notListed[0]?.id).toBe(11);
    expect(notListed[0]?.local).toBe(false);
  });

  it('has one order: newest first, a waiting local where its time says', () => {
    const ordered = mergeMonologs(
      [
        { id: 1, name: 'old', created: '2026-11-12T10:00:00Z' },
        { id: 2, name: 'new', created: '2026-11-12T11:00:00Z' },
      ],
      [{ localId: -1, id: -1, name: 'local', created: '2026-11-12T12:00:00Z' }],
    );
    expect(ordered.map((monolog) => monolog.id)).toEqual([-1, 2, 1]);
  });

  it('allocates a local monolog and goes down', async () => {
    const monologs = bag<Monolog>();
    const cache = cacheWith(monologs);
    const first = await cache.allocate('Первый');
    const second = await cache.allocate('Второй');
    expect(first.id).toBe(-1);
    expect(second.id).toBe(-2);
    expect(first.name).toBe('Первый');
    expect(first.created).toBe('2026-11-12T15:00:00+05:00');
    const local = await cache.localMonologs();
    expect(local.length).toBe(2);
    await cache.mapLocal(-1, 42);
    expect(await cache.serverId(-1)).toBe(42);
    expect(await cache.serverId(-2)).toBeNull();
  });

  it('caches server views separately from local monologs', async () => {
    const cache = cacheWith();
    await cache.allocate('Ждёт');
    await cache.putServerMonologs([{ id: 1, name: 'Работа', count: 2 }], false);
    expect((await cache.monologPage(false, 0, 100)).monologs.some((item) => item.id === 1)).toBe(true);
    await cache.putServerMonologs([{ id: 1, name: 'Работа', count: 2, archived: true }], true);
    expect((await cache.monologPage(false, 0, 100)).monologs.some((item) => item.id === 1)).toBe(false);
    expect((await cache.monologPage(true, 0, 100)).monologs.some((item) => item.id === 1)).toBe(true);
    await cache.putServerMonologs(
      [
        { id: 3, name: 'Ещё', count: 0 },
        { id: 4, name: 'Старое' },
      ],
      false,
    );
    await cache.putServerMonologs([{ id: 3, name: 'Ещё', count: 0 }], false, { complete: true });
    expect(
      (await cache.monologPage(false, 0, 100)).monologs
        .filter((item) => item.id > 0)
        .map((item) => item.id),
    ).toEqual([3]);
    expect((await cache.localMonologs()).length).toBe(1);
  });

  it('is read one page at a time', async () => {
    const cache = cacheWith();
    await cache.putServerMonologs(
      [
        { id: 1, name: 'one', created: '2026-11-12T10:00:00+05:00' },
        { id: 2, name: 'two', created: '2026-11-12T11:00:00+05:00' },
        { id: 3, name: 'three', created: '2026-11-12T12:00:00+05:00' },
      ],
      false,
    );
    await cache.allocate('local');
    const first = await cache.monologPage(false, 0, 2);
    expect(first.monologs.map((item) => item.id)).toEqual([-1, 3]);
    expect(first.total).toBe(4);
    expect(first.hasMore).toBe(true);
    const second = await cache.monologPage(false, 2, 2);
    expect(second.monologs.map((item) => item.id)).toEqual([2, 1]);
    expect(second.hasMore).toBe(false);
  });
});

describe('the feed cache', () => {
  it('is cached per monolog and read in pages', async () => {
    const cache = cacheWith();
    await cache.putMessages(4, [
      { id: 1, recorded, text: 'привет' },
      { id: 2, recorded: later, text: 'ещё' },
    ]);
    const latest = await cache.messagePage(4, { limit: 1 });
    expect(latest.messages.length).toBe(1);
    expect(latest.messages[0]?.text).toBe('ещё');
    const older = await cache.messagePage(4, { before: { recorded: later, id: 2 }, limit: 5 });
    expect(older.messages.length).toBe(1);
    expect(older.messages[0]?.text).toBe('привет');
    expect(await cache.messageCount(4)).toBe(2);
    expect((await cache.messagePage(5, { limit: 5 })).messages.length).toBe(0);
    expect(await cache.hasData()).toBe(true);
    await cache.deleteMessage(4, 2);
    expect(await cache.messageCount(4)).toBe(1);
  });

  it('keeps the queue contract the sync loop expects', () => {
    const cache = cacheWith();
    // Compile-time check: no adapter between the cache and the sync loop.
    const syncCache: SyncCache = cache;
    expect(syncCache).toBe(cache);
  });
});

describe('boot', () => {
  it('a paired phone always keeps the page', () => {
    expect(bootOutcome({ hasToken: true, offline: true, hasCache: true })).toBe('app');
    expect(bootOutcome({ hasToken: true, offline: true, hasCache: false })).toBe('app');
    expect(bootOutcome({ hasToken: false, offline: true, hasCache: true })).toBe('pair');
    expect(bootOutcome({ hasToken: true, offline: false, hasCache: false })).toBe('app');
  });
});

describe('a rename', () => {
  it('never waits for the network', async () => {
    const cache = cacheWith();
    const local = await cache.allocate('Старое');
    expect(await cache.rename(local.id, 'Новое')).toBe(true);
    const localNow = (await cache.localMonologs())[0];
    expect(localNow?.name).toBe('Новое');
    expect((await cache.dirtyRenames()).length).toBe(0);

    await cache.putServerMonologs([{ id: 3, name: 'Серверное' }], false);
    expect(await cache.rename(3, 'Правка')).toBe(true);
    expect(
      (await cache.monologPage(false, 0, 100)).monologs.find((item) => item.id === 3)?.name,
    ).toBe('Правка');
    const waiting = await cache.dirtyRenames();
    expect(waiting.length).toBe(1);
    expect(waiting[0]?.id).toBe(3);

    // A list fetched after the rename must not undo it.
    await cache.putServerMonologs([{ id: 3, name: 'Серверное' }], false);
    expect(
      (await cache.monologPage(false, 0, 100)).monologs.find((item) => item.id === 3)?.name,
    ).toBe('Правка');
  });

  it('made as an override is visible in the list and clear restores the server name', async () => {
    const cache = cacheWith();
    await cache.putServerMonologs([{ id: 3, name: 'Серверное' }], false);
    await cache.applyRename({ id: 3 }, 'Правка');
    expect(
      (await cache.monologPage(false, 0, 100)).monologs.find((item) => item.id === 3)?.name,
    ).toBe('Правка');
    await cache.clearOverride('monolog', 3);
    expect(
      (await cache.monologPage(false, 0, 100)).monologs.find((item) => item.id === 3)?.name,
    ).toBe('Серверное');
  });

  it('of a local monolog is visible before it has a server id', async () => {
    const cache = cacheWith();
    const local = await cache.allocate('Старое');
    await cache.applyRename(local, 'Новое');
    expect((await cache.localMonologs()).find((item) => item.id === local.id)?.name).toBe('Новое');
    expect((await cache.overrides()).length).toBe(1);
    expect((await cache.overrides())[0]).toMatchObject({
      kind: 'monolog',
      id: local.id,
      name: 'Новое',
    });
  });
});

describe('archiving', () => {
  it('moves a monolog between the views at once (stored rename path)', async () => {
    const cache = cacheWith();
    await cache.putServerMonologs([{ id: 5, name: 'Работа' }], false);
    expect((await cache.monologPage(false, 0, 100)).total).toBe(1);
    expect(await cache.setArchived(5, true)).toBe(true);
    expect((await cache.monologPage(false, 0, 100)).total).toBe(0);
    expect((await cache.monologPage(true, 0, 100)).total).toBe(1);
    await cache.setArchived(5, false);
    expect((await cache.monologPage(false, 0, 100)).total).toBe(1);
  });

  it('as an override moves a monolog between the views and clear restores it', async () => {
    const cache = cacheWith();
    await cache.putServerMonologs([{ id: 5, name: 'Работа' }], false);
    await cache.applyArchive({ id: 5 }, true);
    expect((await cache.monologPage(false, 0, 100)).monologs.some((item) => item.id === 5)).toBe(false);
    expect((await cache.monologPage(true, 0, 100)).monologs.some((item) => item.id === 5)).toBe(true);
    await cache.clearOverride('monolog', 5);
    expect((await cache.monologPage(false, 0, 100)).monologs.some((item) => item.id === 5)).toBe(true);
    expect((await cache.monologPage(true, 0, 100)).monologs.some((item) => item.id === 5)).toBe(false);
  });

  it('of a local monolog puts it in the archive list, not the open one', async () => {
    const cache = cacheWith();
    const local = await cache.allocate('Местный');
    await cache.applyArchive(local, true);
    expect(
      (await cache.monologPage(false, 0, 100)).monologs.some((item) => item.id === local.id),
    ).toBe(false);
    expect(
      (await cache.monologPage(true, 0, 100)).monologs.some((item) => item.id === local.id),
    ).toBe(true);
  });

  it('survives a complete open list that has not heard about it', async () => {
    const cache = cacheWith();
    await cache.putServerMonologs([{ id: 5, name: 'Работа' }], false);
    await cache.applyArchive({ id: 5 }, true);
    // The server still calls it open, and this complete open page omits it.
    await cache.putServerMonologs([{ id: 6, name: 'Другое' }], false, { complete: true });
    expect((await cache.monologPage(true, 0, 100)).monologs.some((item) => item.id === 5)).toBe(true);
  });

  it('follows a local monolog across its server id', async () => {
    const cache = cacheWith();
    const local = await cache.allocate('Старое');
    await cache.applyRename(local, 'Новое');
    await cache.mapLocal(local.id, 42);
    await cache.putServerMonologs([{ id: 42, name: 'Серверное' }], false);
    expect(
      (await cache.monologPage(false, 0, 100)).monologs.find((item) => item.id === 42)?.name,
    ).toBe('Новое');
    expect((await cache.overrides()).find((override) => override.kind === 'monolog')?.id).toBe(42);
  });
});

describe('an edit', () => {
  it('returns the phone text from pages and message; clear restores the server text', async () => {
    const messages = messageStore();
    const cache = cacheWith(bag<Monolog>(), messages);
    await cache.putMessages(4, [{ id: 1, recorded, text: 'server' }]);
    const target = { id: 1, monolog: 4, recorded, text: 'server' };
    await cache.applyEdit(target, 'edited');

    expect((await cache.messagePage(4, { limit: 5 })).messages[0]?.text).toBe('edited');
    expect((await cache.message(4, 1))?.text).toBe('edited');
    // The server row itself is untouched.
    expect(messages.rows.get('4|' + recorded + '|1')?.text).toBe('server');

    // A later server page with the old text does not undo the edit.
    await cache.putMessages(4, [{ id: 1, recorded, text: 'server' }]);
    expect((await cache.messagePage(4, { limit: 5 })).messages[0]?.text).toBe('edited');

    await cache.clearOverride('message', 1);
    expect((await cache.messagePage(4, { limit: 5 })).messages[0]?.text).toBe('server');
    expect(await cache.overrides()).toEqual([]);
  });

  it('survives a reload (a new cache over the same store)', async () => {
    const monologs = bag<Monolog>();
    const messages = messageStore();
    const local = localBag();
    const before = cacheWith(monologs, messages, local);
    await before.putMessages(4, [{ id: 1, recorded, text: 'server' }]);
    await before.applyEdit({ id: 1, monolog: 4, recorded }, 'edited');

    const after = cacheWith(monologs, messages, local);
    expect((await after.messagePage(4, { limit: 5 })).messages[0]?.text).toBe('edited');
  });
});

describe('a delete', () => {
  it('hides the message from pages, message and the count but keeps its row', async () => {
    const messages = messageStore();
    const cache = cacheWith(bag<Monolog>(), messages);
    await cache.putMessages(4, [
      { id: 1, recorded, text: 'one' },
      { id: 2, recorded: later, text: 'two' },
    ]);
    await cache.applyDelete({ id: 1, monolog: 4, recorded });

    expect((await cache.messagePage(4, { limit: 5 })).messages.map((message) => message.id)).toEqual([
      2,
    ]);
    expect(await cache.message(4, 1)).toBeNull();
    expect(await cache.messageCount(4)).toBe(1);
    // The tombstone hides the row; it never deletes it.
    expect(messages.rows.has('4|' + recorded + '|1')).toBe(true);

    // A server page that still contains the message cannot bring it back.
    await cache.putMessages(4, [{ id: 1, recorded, text: 'one' }]);
    expect((await cache.messagePage(4, { limit: 5 })).messages.some((m) => m.id === 1)).toBe(false);
    expect(await cache.messageCount(4)).toBe(1);

    // The sync loop drops the tombstone only after the computer confirmed the
    // delete, and deletes the cached row then, knowing ids only.
    await cache.deleteMessage(4, 1);
    expect(messages.rows.has('4|' + recorded + '|1')).toBe(false);
    expect(await cache.overrides()).toEqual([]);
  });

  it('leaves the other monologs alone', async () => {
    // Message ids are global on the computer, so a tombstone is keyed by id
    // alone; a different message in another monolog keeps its own row.
    const cache = cacheWith();
    await cache.putMessages(4, [{ id: 1, recorded, text: 'four' }]);
    await cache.putMessages(9, [{ id: 2, recorded, text: 'nine' }]);
    await cache.applyDelete({ id: 1, monolog: 4, recorded });
    expect(await cache.messageCount(4)).toBe(0);
    expect(await cache.messageCount(9)).toBe(1);
  });

  it('finds the row by ids alone when the computer confirmed the delete', async () => {
    // A real recording stamp: the caller (the sync loop) never has it, only
    // the monolog and message ids.
    const messages = messageStore();
    const cache = cacheWith(bag<Monolog>(), messages);
    await cache.putMessages(4, [
      { id: 7, monolog: 4, recorded, text: 'goes away' },
      { id: 8, monolog: 4, recorded: later, text: 'stays' },
    ]);
    await cache.applyDelete({ id: 7, monolog: 4, recorded });
    expect((await cache.messagePage(4, { limit: 10 })).messages.map((m) => m.id)).toEqual([8]);

    await cache.deleteMessage(4, 7);

    expect(messages.rows.has('4|' + recorded + '|7')).toBe(false);
    expect(await messages.find(4, 7)).toBeNull();
    expect(await cache.overrides()).toEqual([]);
    expect((await cache.messagePage(4, { limit: 10 })).messages.map((m) => m.id)).toEqual([8]);
    expect(await cache.messageCount(4)).toBe(1);
  });

  it('hides a monolog tombstone from a later page, then drops row and tombstone', async () => {
    const monologs = bag<Monolog>();
    const messages = messageStore();
    const cache = cacheWith(monologs, messages);
    await cache.putServerMonologs([{ id: 3, name: 'Работа' }], false);
    await cache.putMessages(3, [{ id: 7, monolog: 3, recorded, text: 'текст' }]);
    await cache.applyDeleteMonolog({ id: 3 });

    expect((await cache.monologPage(false, 0, 100)).monologs).toEqual([]);
    expect(await cache.serverMonologCount(false)).toBe(0);

    // A complete page fetched after the tombstone cannot bring it back.
    await cache.putServerMonologs([{ id: 3, name: 'Работа' }], false, { complete: true });
    expect((await cache.monologPage(false, 0, 100)).monologs).toEqual([]);

    // The computer accepted: the row, its cached messages and both tombstones
    // go together.
    await cache.deleteMonolog(3);
    expect(await cache.overrides()).toEqual([]);
    expect(await messages.find(3, 7)).toBeNull();
    expect(monologs.rows.size).toBe(0);
  });
});

describe('a change the computer accepted', () => {
  it('commitEdit writes the row, drops the override, and never exposes stale text', async () => {
    const messages = messageStore();
    const cache = cacheWith(bag<Monolog>(), messages);
    await cache.putMessages(4, [{ id: 1, monolog: 4, recorded, text: 'server' }]);
    await cache.applyEdit({ id: 1, monolog: 4, recorded }, 'edited');

    // A read that slips in while the row is being written must still see the
    // override (the accepted text), not the old server row.
    const during: number[] = [];
    const write = messages.put.bind(messages);
    messages.put = async (list = []) => {
      during.push((await cache.overrides()).length);
      return write(list);
    };

    await cache.commitEdit(4, 1, 'accepted');

    expect(during).toEqual([1]);
    expect((await cache.messagePage(4, { limit: 5 })).messages[0]?.text).toBe('accepted');
    expect(await cache.overrides()).toEqual([]);
    expect(messages.rows.get('4|' + recorded + '|1')?.text).toBe('accepted');
  });

  it('commitRename writes the name into the row and drops the override', async () => {
    const cache = cacheWith();
    await cache.putServerMonologs([{ id: 3, name: 'Серверное' }], false);
    await cache.applyRename({ id: 3 }, 'Правка');

    await cache.commitRename(3, 'Правка');

    expect(
      (await cache.monologPage(false, 0, 100)).monologs.find((item) => item.id === 3)?.name,
    ).toBe('Правка');
    expect(await cache.overrides()).toEqual([]);
  });

  it('commitArchive writes the list into the row and drops the override', async () => {
    const cache = cacheWith();
    await cache.putServerMonologs([{ id: 5, name: 'Работа' }], false);
    await cache.applyArchive({ id: 5 }, true);

    await cache.commitArchive(5, true);

    expect((await cache.monologPage(false, 0, 100)).monologs.some((item) => item.id === 5)).toBe(
      false,
    );
    expect((await cache.monologPage(true, 0, 100)).monologs.some((item) => item.id === 5)).toBe(
      true,
    );
    expect(await cache.overrides()).toEqual([]);
  });

  it('a commit on a row that was never cached does not throw and clears the override', async () => {
    const cache = cacheWith();

    await cache.applyEdit({ id: 99, monolog: 4, recorded }, 'edited');
    await cache.commitEdit(4, 99, 'accepted');
    expect(await cache.overrides()).toEqual([]);

    await cache.applyRename({ id: 99 }, 'Новое');
    await cache.commitRename(99, 'Новое');
    expect(await cache.overrides()).toEqual([]);

    await cache.applyArchive({ id: 99 }, true);
    await cache.commitArchive(99, true);
    expect(await cache.overrides()).toEqual([]);
  });
});

describe('overrides', () => {
  it('merge into one record per row and list back', async () => {
    const cache = cacheWith();
    await cache.putServerMonologs([{ id: 5, name: 'Работа' }], false);
    const first = await cache.applyRename({ id: 5 }, 'Дом');
    const second = await cache.applyArchive({ id: 5 }, true);
    expect(first.id).toBe(5);
    expect(second.at).toBe('2026-11-12T15:00:00+05:00');
    const all = await cache.overrides();
    expect(all.length).toBe(1);
    expect(all[0]).toMatchObject({ kind: 'monolog', id: 5, name: 'Дом', archived: true });

    await cache.clearOverride('monolog', 5);
    expect(await cache.overrides()).toEqual([]);
  });
});
