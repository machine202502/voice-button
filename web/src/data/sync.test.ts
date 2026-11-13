// Tests for the phone's sync loop, without a browser and without a network.
//
//   vitest run src/data
//
// The loop is where the phone used to hang, so every case that used to break is
// written down here: a flush that fails, a flush that is asked for while one is
// running, a take that was never closed, an acknowledged chunk. Then the change
// queue: one order, no overtaking, a lost connection leaves it alone, a refusal
// is reported and dropped, and only a confirmed delete removes the cached row.

import { describe, expect, it } from 'vitest';

import type { MessagePage } from './cache';
import { createMutations } from './mutations';
import type { MutationQueue } from './mutations';
import type { MutationStore } from './storage';
import { createSync, messageCursor } from './sync';
import type { Api, ApiAnswer, Chunk, ListResult, Outbox, Sync, SyncCache } from './sync';
import type { EntityId, LocalMonolog, Message, Monolog, MutationRecord, Take } from './types';

// ---------------------------------------------------------------- fakes

type TakeWithChunks = Partial<Take> & { localId: string; chunks?: Chunk[] };

/** One take with the fields the canonical shape requires. */
function take(parts: TakeWithChunks): Take {
  return {
    localId: parts.localId,
    monolog: parts.monolog ?? 1,
    recorded: parts.recorded ?? '2026-11-12T15:00:00+05:00',
    remote: parts.remote === undefined ? null : parts.remote,
    closed: parts.closed ?? false,
    seq: parts.seq ?? 0,
  };
}

function fakeOutbox(items: TakeWithChunks[] = []) {
  const state = new Map<string, Take & { chunks: Chunk[] }>();
  for (const item of items) {
    state.set(item.localId, { ...take(item), chunks: [...(item.chunks ?? [])] });
  }
  const outbox: Outbox = {
    async list() {
      const takes: Take[] = [];
      for (const item of state.values()) {
        takes.push({
          localId: item.localId,
          monolog: item.monolog,
          recorded: item.recorded,
          remote: item.remote,
          closed: item.closed,
          seq: item.seq,
        });
      }
      return takes;
    },
    async put(next) {
      state.set(next.localId, {
        ...next,
        chunks: state.get(next.localId)?.chunks ?? [],
      });
    },
    async remove(localId) {
      state.delete(localId);
    },
    async chunks(localId) {
      return state.get(localId)?.chunks ?? [];
    },
    async dropChunk(localId, seq) {
      const item = state.get(localId);
      if (item) item.chunks = item.chunks.filter((chunk) => chunk.seq !== seq);
    },
  };
  return { outbox, state };
}

interface FakeApi {
  calls: string[];
  bodies: unknown[];
  state: { fail: boolean };
  api: Api;
}

/** Records every call and answers like the computer would. */
function fakeApi({ messages = [] as Message[] } = {}): FakeApi {
  const calls: string[] = [];
  const bodies: unknown[] = [];
  const state = { fail: false };
  let nextId = 100;
  const api: Api = async (method, path, body) => {
    if (state.fail) throw new Error('offline');
    calls.push(method + ' ' + path);
    bodies.push(body);
    if (method === 'POST' && path === '/messages') return { id: nextId++ };
    if (method === 'GET' && path.startsWith('/monologues?')) {
      return { monologs: [], total: 0, has_more: false };
    }
    if (method === 'GET' && path.startsWith('/monologues/')) {
      return { messages, total: messages.length, has_more: false };
    }
    return { ok: true };
  };
  return { calls, bodies, state, api };
}

/** A computer with a monolog and a feed; one call can be made to fail. */
function monologApi({ failMonolog = false } = {}) {
  const calls: string[] = [];
  const state = { failAll: false };
  let nextMessage = 100;
  const api: Api = async (method, path, body) => {
    if (state.failAll) throw new Error('offline');
    if (method === 'POST' && path === '/monologues') {
      if (failMonolog) throw new Error('offline');
      calls.push('POST /monologues ' + JSON.stringify(body));
      return { ok: true, monolog: { id: 77, name: (body as { name?: string }).name } };
    }
    calls.push(method + ' ' + path);
    if (method === 'POST' && path === '/messages') return { id: nextMessage++ };
    if (method === 'GET' && path.startsWith('/monologues?')) {
      return { monologs: [{ id: 77, name: 'Новый', count: 1 }], total: 1, has_more: false };
    }
    if (method === 'GET' && path.startsWith('/monologues/')) {
      return { messages: [], total: 0, has_more: false };
    }
    return { ok: true };
  };
  return { calls, state, api };
}

interface Commit {
  method: 'edit' | 'rename' | 'archive';
  monolog: number;
  message?: number;
  value: unknown;
}

interface FakeCache extends SyncCache {
  locals: Map<number, { name: string; serverId: number | null }>;
  rows: Map<string, Message>;
  monologs: Map<number, Monolog>;
  deleted: Array<{ monolog: number; id: number }>;
  commits: Commit[];
  /** Called at the start of every commit; lets a test look at the queue then. */
  onCommit?: () => void | Promise<void>;
}

function rowKey(monolog: number, id: number, recorded: string): string {
  return monolog + '|' + recorded + '|' + id;
}

/** A row a page can be compared against: a full message or a cursor. */
type CursorRow = { id: EntityId; recorded?: string };

function compareMessages(left: CursorRow, right: CursorRow): number {
  const a = String(left.recorded ?? '');
  const b = String(right.recorded ?? '');
  if (a !== b) return a < b ? -1 : 1;
  return Number(left.id) - Number(right.id);
}

function fakeCache(): FakeCache {
  const cache: FakeCache = {
    locals: new Map(),
    rows: new Map(),
    monologs: new Map(),
    deleted: [],
    commits: [],
    async localMonologs() {
      const found: LocalMonolog[] = [];
      for (const [localId, record] of cache.locals) {
        found.push({
          id: localId,
          localId,
          name: record.name,
          serverId: record.serverId,
          local: true,
        });
      }
      return found;
    },
    async mapLocal(localId, serverId) {
      const record = cache.locals.get(Number(localId));
      if (record) record.serverId = serverId;
    },
    async serverId(localId) {
      return cache.locals.get(Number(localId))?.serverId ?? null;
    },
    async putMessages(monologId, list = []) {
      for (const message of list) {
        const id = Number(message.id);
        if (!Number.isFinite(id)) continue;
        const recorded = String(message.recorded ?? '');
        cache.rows.set(rowKey(monologId, id, recorded), {
          ...message,
          id,
          monolog: monologId,
          recorded,
        });
      }
      return list.length;
    },
    async messagePage(monologId, { before = null, after = null, limit = 50 } = {}) {
      const all = [...cache.rows.values()]
        .filter((message) => message.monolog === monologId)
        .sort(compareMessages);
      const size = Math.max(1, Number(limit) || 50);
      if (before) {
        const older = all.filter((message) => compareMessages(message, before) < 0);
        const chunk = older.slice(-(size + 1));
        return { messages: chunk.slice(-size), hasMore: chunk.length > size, total: null };
      }
      if (after) {
        const newer = all.filter((message) => compareMessages(message, after) > 0);
        return { messages: newer.slice(0, size), hasMore: newer.length > size, total: null };
      }
      const chunk = all.slice(-(size + 1));
      return { messages: chunk.slice(-size), hasMore: chunk.length > size, total: null };
    },
    async messageCount(monologId) {
      let total = 0;
      for (const message of cache.rows.values()) {
        if (message.monolog === monologId) total += 1;
      }
      return total;
    },
    async putServerMonologs(list = [], archived = false) {
      for (const monolog of list) {
        const id = Number(monolog.id);
        if (!Number.isFinite(id)) continue;
        cache.monologs.set(id, {
          ...monolog,
          id,
          name: typeof monolog.name === 'string' ? monolog.name : '',
          archived,
        });
      }
    },
    async deleteMessage(monologId, messageId) {
      await cache.onCommit?.();
      const id = Number(messageId);
      cache.deleted.push({ monolog: monologId, id });
      for (const [key, row] of cache.rows) {
        if (row.monolog === monologId && row.id === id) cache.rows.delete(key);
      }
    },
    async commitEdit(monologId, messageId, text) {
      await cache.onCommit?.();
      cache.commits.push({
        method: 'edit',
        monolog: monologId,
        message: Number(messageId),
        value: text,
      });
    },
    async commitRename(monologId, name) {
      await cache.onCommit?.();
      cache.commits.push({ method: 'rename', monolog: monologId, value: name });
    },
    async commitArchive(monologId, archived) {
      await cache.onCommit?.();
      cache.commits.push({ method: 'archive', monolog: monologId, value: archived });
    },
  };
  return cache;
}

/** The change queue over a map, with the real queue module. */
function mutationQueue() {
  const rows = new Map<number, MutationRecord>();
  let next = 1;
  const store: MutationStore = {
    async put(value) {
      const seq = next;
      next += 1;
      rows.set(seq, { ...value });
      return seq;
    },
    async get(key) {
      return rows.get(key);
    },
    async remove(key) {
      rows.delete(key);
    },
    async keys() {
      return [...rows.keys()];
    },
  };
  const mutations: MutationQueue = createMutations({
    store,
    now: () => '2026-11-13T10:00:00+05:00',
  });
  return { mutations, rows };
}

interface SyncParts {
  api: Api;
  outbox: Outbox;
  cache: SyncCache;
  mutations: MutationQueue;
  render?: (page: MessagePage, pending: Take[]) => void | Promise<void>;
  renderList?: (page: ListResult, archived: boolean) => void | Promise<void>;
  mapped?: (localId: number, serverId: number) => void;
}

function makeSync(parts: SyncParts): Sync {
  return createSync({
    api: parts.api,
    outbox: parts.outbox,
    cache: parts.cache,
    mutations: parts.mutations,
    render: parts.render ?? (() => {}),
    renderList: parts.renderList,
    mapped: parts.mapped,
    log: () => {},
  });
}

/** An empty feed plus an empty queue over a fresh cache. */
function emptySync(overrides: Partial<SyncParts> = {}): Sync {
  const { outbox } = fakeOutbox();
  return makeSync({
    api: overrides.api ?? fakeApi().api,
    outbox: overrides.outbox ?? outbox,
    cache: overrides.cache ?? fakeCache(),
    mutations: overrides.mutations ?? mutationQueue().mutations,
    render: overrides.render,
    renderList: overrides.renderList,
    mapped: overrides.mapped,
  });
}

// ---------------------------------------------------------------- the loop

describe('the sync loop', () => {
  it('sends a take and closes it only after the computer agreed', async () => {
    const { outbox, state } = fakeOutbox([
      {
        localId: 'a',
        monolog: 1,
        remote: 7,
        closed: true,
        chunks: [
          { seq: 0, bytes: new Uint8Array([1, 0]) },
          { seq: 1, bytes: new Uint8Array([2, 0]) },
        ],
      },
    ]);
    const api = fakeApi();
    let drawn: MessagePage | null = null;
    const sync = makeSync({
      api: api.api,
      outbox,
      cache: fakeCache(),
      mutations: mutationQueue().mutations,
      render: (page) => {
        drawn = page;
      },
    });
    sync.show(1);
    const result = await sync.tick();
    expect(result.sent).toBe(1);
    expect(api.calls.join('|')).toBe(
      'POST /messages/7/audio?seq=0|POST /messages/7/audio?seq=1|POST /messages/7/finish|GET /monologues/1/messages?limit=50',
    );
    expect(state.size).toBe(0);
    expect(drawn).not.toBeNull();
  });

  it('does not close a take that is still recording', async () => {
    const { outbox, state } = fakeOutbox([
      { localId: 'b', monolog: 1, remote: 8, closed: false, chunks: [] },
    ]);
    const api = fakeApi();
    const sync = makeSync({
      api: api.api,
      outbox,
      cache: fakeCache(),
      mutations: mutationQueue().mutations,
    });
    sync.show(1);
    await sync.tick();
    expect(api.calls.some((call) => call.includes('finish'))).toBe(false);
    expect(state.size).toBe(1);
  });

  it('gives a take with no id one before its audio', async () => {
    const { outbox, state } = fakeOutbox([
      { localId: 'c', monolog: 1, remote: null, closed: true, chunks: [] },
    ]);
    const api = fakeApi();
    const sync = makeSync({
      api: api.api,
      outbox,
      cache: fakeCache(),
      mutations: mutationQueue().mutations,
    });
    sync.show(1);
    await sync.tick();
    expect(api.calls[0]).toBe('POST /messages');
    expect(api.calls.includes('POST /messages/100/finish')).toBe(true);
    expect(state.size).toBe(0);
  });

  it('leaves nothing stuck after a failed flush', async () => {
    const { outbox, state } = fakeOutbox([
      {
        localId: 'd',
        monolog: 1,
        remote: 9,
        closed: true,
        chunks: [{ seq: 0, bytes: new Uint8Array([1, 0]) }],
      },
    ]);
    const api = fakeApi();
    api.state.fail = true;
    const sync = makeSync({
      api: api.api,
      outbox,
      cache: fakeCache(),
      mutations: mutationQueue().mutations,
    });
    sync.show(1);
    const first = await sync.tick();
    expect(first.failed).toBe(true);
    expect(state.size).toBe(1);
    api.state.fail = false;
    const second = await sync.tick();
    expect(second.failed).not.toBe(true);
    expect(state.size).toBe(0);
  });

  it('draws the take that waits when the flush failed', async () => {
    const { outbox } = fakeOutbox([
      { localId: 'e', monolog: 1, remote: null, closed: true, chunks: [] },
    ]);
    const api = fakeApi();
    api.state.fail = true;
    const drawn: Array<{ page: MessagePage; pending: Take[] }> = [];
    const sync = makeSync({
      api: api.api,
      outbox,
      cache: fakeCache(),
      mutations: mutationQueue().mutations,
      render: (page, pending) => {
        drawn.push({ page, pending });
      },
    });
    sync.show(1);
    const result = await sync.tick();
    expect(result.failed).toBe(true);
    expect(drawn.length).toBe(1);
    expect(drawn[0]?.pending.length).toBe(1);
    expect(drawn[0]?.pending[0]?.localId).toBe('e');
  });

  it('never runs two ticks at once', async () => {
    let inFlight = 0;
    let maxInFlight = 0;
    const api: Api = async () => {
      inFlight += 1;
      maxInFlight = Math.max(maxInFlight, inFlight);
      await new Promise((resolve) => setTimeout(resolve, 20));
      inFlight -= 1;
      return { messages: [] };
    };
    const sync = makeSync({
      api,
      outbox: fakeOutbox().outbox,
      cache: fakeCache(),
      mutations: mutationQueue().mutations,
    });
    sync.show(1);
    const results = await Promise.all([sync.tick(), sync.tick(), sync.tick()]);
    expect(maxInFlight).toBe(1);
    expect(results.filter((result) => result.skipped).length).toBe(2);
  });

  it('fetches nothing while no monolog is open', async () => {
    const api = fakeApi();
    const sync = emptySync({ api: api.api });
    await sync.tick();
    expect(api.calls.length).toBe(0);
  });

  it('creates a local monolog before its take and maps the id', async () => {
    const cache = fakeCache();
    cache.locals.set(-1, { name: 'Новый', serverId: null });
    const { outbox, state } = fakeOutbox([
      {
        localId: 'h',
        monolog: -1,
        remote: null,
        closed: false,
        chunks: [{ seq: 0, bytes: new Uint8Array([1, 0]) }],
      },
    ]);
    const api = monologApi();
    const drawn: MessagePage[] = [];
    const mapped: Array<[number, number]> = [];
    const sync = makeSync({
      api: api.api,
      outbox,
      cache,
      mutations: mutationQueue().mutations,
      mapped: (localId, serverId) => {
        mapped.push([localId, serverId]);
      },
      render: (page) => {
        drawn.push(page);
      },
      renderList: () => {},
    });
    sync.show(-1);
    const result = await sync.tick();
    expect(result.failed).not.toBe(true);
    expect(api.calls.findIndex((call) => call.startsWith('POST /monologues'))).toBeLessThan(
      api.calls.findIndex((call) => call.startsWith('POST /messages')),
    );
    expect(await cache.serverId(-1)).toBe(77);
    expect(state.get('h')?.monolog).toBe(77);
    expect(state.get('h')?.remote).toBe(100);
    expect(mapped).toEqual([[-1, 77]]);
    expect(api.calls.includes('GET /monologues/77/messages?limit=50')).toBe(true);
    expect(api.calls.includes('GET /monologues?archived=0&limit=100&offset=0')).toBe(true);
    expect(cache.monologs.has(77)).toBe(true);

    // Closing the take later completes it under the same server monolog.
    const stored = state.get('h');
    if (stored) stored.closed = true;
    const finished = await sync.tick();
    expect(finished.failed).not.toBe(true);
    expect(state.size).toBe(0);
  });

  it('sends an old take through a mapping stored by an earlier tick', async () => {
    const cache = fakeCache();
    cache.locals.set(-1, { name: 'Новый', serverId: 77 });
    const { outbox, state } = fakeOutbox([
      { localId: 'k', monolog: -1, remote: null, closed: true, chunks: [] },
    ]);
    const api = monologApi();
    const sync = makeSync({
      api: api.api,
      outbox,
      cache,
      mutations: mutationQueue().mutations,
    });
    sync.show(-1);
    const result = await sync.tick();
    expect(result.failed).not.toBe(true);
    expect(api.calls.some((call) => call.startsWith('POST /monologues'))).toBe(false);
    expect(state.size).toBe(0);
    expect(api.calls.includes('GET /monologues/77/messages?limit=50')).toBe(true);
  });

  it('never loses a take while its monolog has no server id', async () => {
    const cache = fakeCache();
    cache.locals.set(-1, { name: 'Ждёт', serverId: null });
    const { outbox, state } = fakeOutbox([
      {
        localId: 'i',
        monolog: -1,
        remote: null,
        closed: true,
        chunks: [{ seq: 0, bytes: new Uint8Array([1, 0]) }],
      },
    ]);
    let server = monologApi({ failMonolog: true });
    const drawn: Array<{ pending: Take[] }> = [];
    const sync = makeSync({
      api: (method, path, body) => server.api(method, path, body),
      outbox,
      cache,
      mutations: mutationQueue().mutations,
      render: (_page, pending) => {
        drawn.push({ pending });
      },
    });
    sync.show(-1);
    const first = await sync.tick();
    expect(first.failed).toBe(true);
    expect(state.size).toBe(1);
    expect(state.get('i')?.monolog).toBe(-1);
    expect(server.calls.some((call) => call.startsWith('POST /messages'))).toBe(false);
    expect(drawn[0]?.pending.length).toBe(1);

    server = monologApi();
    const second = await sync.tick();
    expect(second.failed).not.toBe(true);
    expect(state.size).toBe(0);
    expect(await cache.serverId(-1)).toBe(77);
  });

  it('draws the cached messages while the computer is away', async () => {
    const cache = fakeCache();
    await cache.putMessages(1, [{ id: 5, stage: 'done', text: 'старое', recorded: 'x' }]);
    const { outbox } = fakeOutbox([
      { localId: 'j', monolog: 1, remote: null, closed: true, chunks: [] },
    ]);
    const api = monologApi();
    api.state.failAll = true;
    const drawn: Array<{ page: MessagePage; pending: Take[] }> = [];
    const sync = makeSync({
      api: api.api,
      outbox,
      cache,
      mutations: mutationQueue().mutations,
      render: (page, pending) => {
        drawn.push({ page, pending });
      },
    });
    sync.show(1);
    const result = await sync.tick();
    expect(result.failed).toBe(true);
    expect(drawn[0]?.page.messages.length).toBe(1);
    expect(drawn[0]?.page.messages[0]?.text).toBe('старое');
    expect(drawn[0]?.pending.length).toBe(1);
  });

  it('fetches, caches and draws the list', async () => {
    const cache = fakeCache();
    const api = monologApi();
    const drawn: ListResult[] = [];
    const sync = makeSync({
      api: api.api,
      outbox: fakeOutbox().outbox,
      cache,
      mutations: mutationQueue().mutations,
      renderList: (page) => {
        drawn.push(page);
      },
    });
    const result = await sync.tick();
    expect(result.monologs).toBe(1);
    expect(drawn[0]?.monologs.length).toBe(1);
    expect(cache.monologs.size).toBe(1);
  });

  it('continues a refused take in a new message', async () => {
    const { outbox, state } = fakeOutbox([
      {
        localId: 'refused',
        monolog: 1,
        remote: 5,
        closed: true,
        chunks: [
          { seq: 0, bytes: new Uint8Array([1, 2]) },
          { seq: 1, bytes: new Uint8Array([3, 4]) },
        ],
      },
      {
        localId: 'good',
        monolog: 1,
        remote: 6,
        closed: true,
        chunks: [{ seq: 0, bytes: new Uint8Array([5, 6]) }],
      },
    ]);
    const calls: string[] = [];
    const api: Api = async (method, path) => {
      calls.push(method + ' ' + path);
      if (path.startsWith('/messages/5/audio')) {
        throw Object.assign(new Error('this message is not receiving audio'), { status: 409 });
      }
      if (method === 'POST' && path === '/messages') return { message: { id: 100 } };
      if (method === 'GET') return { monologs: [] };
      return { ok: true };
    };
    const sync = makeSync({
      api,
      outbox,
      cache: fakeCache(),
      mutations: mutationQueue().mutations,
      renderList: () => {},
    });
    const result = await sync.tick();
    expect(result.failed).not.toBe(true);
    expect(calls.includes('POST /messages')).toBe(true);
    expect(calls.includes('POST /messages/100/audio?seq=0')).toBe(true);
    expect(calls.includes('POST /messages/100/audio?seq=1')).toBe(true);
    expect(calls.includes('POST /messages/100/finish')).toBe(true);
    expect(state.has('refused')).toBe(false);
    expect(state.has('good')).toBe(false);
  });

  it('reads a scroll page from the cache before the network', async () => {
    const cache = fakeCache();
    await cache.putMessages(1, [
      { id: 1, recorded: '2026-11-12T10:00:00+05:00', text: 'one' },
      { id: 2, recorded: '2026-11-12T11:00:00+05:00', text: 'two' },
    ]);
    const api = fakeApi();
    const sync = emptySync({ api: api.api, cache });
    const older = await sync.page(1, {
      before: { id: 2, recorded: '2026-11-12T11:00:00+05:00' },
      limit: 1,
    });
    expect(older.messages.length).toBe(1);
    expect(older.messages[0]?.id).toBe(1);
    expect(api.calls.length).toBe(0);
  });

  it('asks the computer for the rest of a short stored page', async () => {
    const cache = fakeCache();
    await cache.putMessages(1, [{ id: 1, recorded: '2026-11-12T10:00:00+05:00', text: 'one' }]);
    const api = fakeApi({
      messages: [
        { id: 0, monolog: 1, recorded: '2026-11-01T10:00:00+05:00', text: 'older' },
        { id: 1, monolog: 1, recorded: '2026-11-12T10:00:00+05:00', text: 'one' },
      ],
    });
    const sync = emptySync({ api: api.api, cache });
    const page = await sync.page(1, {
      before: { id: 1, recorded: '2026-11-12T10:00:00+05:00' },
      limit: 2,
    });
    expect(page.messages.length).toBe(2);
    expect(api.calls.some((call) => call.includes('before='))).toBe(true);
    expect((await cache.messagePage(1, { limit: 10 })).messages.length).toBe(2);
  });

  it('uses recorded|id as the cursor', () => {
    expect(messageCursor({ id: 7, recorded: '2026-11-12T10:00:00+05:00' })).toBe(
      '2026-11-12T10:00:00+05:00|7',
    );
  });
});

// ---------------------------------------------------------------- the queue

describe('the change queue in the loop', () => {
  interface SentCall {
    method: string;
    path: string;
    body: unknown;
  }

  /** An api that records every call and can refuse selected paths. */
  function recordingApi(
    plan: (call: SentCall) => ApiAnswer | Error | Promise<ApiAnswer | Error> = () => ({ ok: true }),
  ) {
    const calls: SentCall[] = [];
    const api: Api = async (method, path, body) => {
      const call = { method, path, body };
      calls.push(call);
      if (method === 'GET' && path.startsWith('/monologues?')) {
        return { monologs: [], total: 0, has_more: false };
      }
      if (method === 'GET' && path.startsWith('/monologues/')) {
        return { messages: [], total: 0, has_more: false };
      }
      const answer = await plan(call);
      if (answer instanceof Error) throw answer;
      return answer;
    };
    return { calls, api, sent: () => calls.filter((call) => call.method !== 'GET') };
  }

  it('drains every kind in the order the person acted', async () => {
    const cache = fakeCache();
    const { mutations } = mutationQueue();
    await mutations.put({
      kind: 'monolog.rename',
      target: { monolog: 1 },
      payload: { name: 'Дом' },
      local: false,
    });
    await mutations.put({
      kind: 'monolog.archive',
      target: { monolog: 1 },
      payload: { archived: true },
      local: false,
    });
    await mutations.put({
      kind: 'message.edit',
      target: { monolog: 1, message: 5 },
      payload: { text: 'новый' },
      local: false,
    });
    await mutations.put({
      kind: 'message.delete',
      target: { monolog: 1, message: 6 },
      payload: {},
      local: false,
    });
    const recorder = recordingApi();
    // The queue count seen by each commit: if a commit ran after the remove,
    // the count would already have dropped.
    const queuedAtCommit: number[] = [];
    cache.onCommit = async () => {
      queuedAtCommit.push(await mutations.count());
    };
    const sync = emptySync({ api: recorder.api, cache, mutations, renderList: () => {} });
    const result = await sync.tick();
    expect(result.refused).toEqual([]);
    expect(recorder.sent()).toEqual([
      { method: 'PATCH', path: '/monologues/1', body: { name: 'Дом' } },
      { method: 'POST', path: '/monologues/1/archive', body: { archived: true } },
      { method: 'PATCH', path: '/messages/5', body: { text: 'новый' } },
      { method: 'DELETE', path: '/messages/6', body: undefined },
    ]);
    expect(await mutations.count()).toBe(0);
    // Every accepted change is written into the cached row, which clears the
    // override; the confirmed delete removes row and tombstone instead.
    expect(cache.commits).toEqual([
      { method: 'rename', monolog: 1, value: 'Дом' },
      { method: 'archive', monolog: 1, value: true },
      { method: 'edit', monolog: 1, message: 5, value: 'новый' },
    ]);
    expect(cache.deleted).toEqual([{ monolog: 1, id: 6 }]);
    // Every commit ran while its mutation was still in the queue, so a failed
    // cache write keeps it there for the retry.
    expect(queuedAtCommit).toEqual([4, 3, 2, 1]);
  });

  it('leaves the queue intact when the connection is lost', async () => {
    const cache = fakeCache();
    const { mutations } = mutationQueue();
    await mutations.put({
      kind: 'monolog.rename',
      target: { monolog: 1 },
      payload: { name: 'Первый' },
      local: false,
    });
    await mutations.put({
      kind: 'monolog.archive',
      target: { monolog: 1 },
      payload: { archived: true },
      local: false,
    });
    let broken = true;
    const recorder = recordingApi((call) => {
      if (broken && call.path === '/monologues/1/archive') return new Error('offline');
      return { ok: true };
    });
    const sync = emptySync({ api: recorder.api, cache, mutations, renderList: () => {} });
    await sync.tick();
    // The rename went through before the link died: the archive is still first
    // in line and the queue kept it.
    expect(await mutations.count()).toBe(1);
    expect((await mutations.take())?.seq).toBe(2);
    expect(recorder.sent().map((call) => call.path)).toEqual([
      '/monologues/1',
      '/monologues/1/archive',
    ]);
    // Only the accepted rename was committed; the failed archive keeps its
    // override for the retry.
    expect(cache.commits).toEqual([{ method: 'rename', monolog: 1, value: 'Первый' }]);
    broken = false;
    const result = await sync.tick();
    expect(result.failed).not.toBe(true);
    expect(result.refused).toEqual([]);
    expect(await mutations.count()).toBe(0);
    expect(cache.commits).toEqual([
      { method: 'rename', monolog: 1, value: 'Первый' },
      { method: 'archive', monolog: 1, value: true },
    ]);
    expect(recorder.sent().map((call) => call.path)).toEqual([
      '/monologues/1',
      '/monologues/1/archive',
      '/monologues/1/archive',
    ]);
  });

  it('drops a refused delete, reports it and keeps the cached row', async () => {
    const cache = fakeCache();
    await cache.putMessages(1, [{ id: 5, recorded: 'x', text: 'поздно' }]);
    const { mutations } = mutationQueue();
    await mutations.put({
      kind: 'message.delete',
      target: { monolog: 1, message: 5 },
      payload: {},
      local: false,
    });
    await mutations.put({
      kind: 'message.edit',
      target: { monolog: 1, message: 6 },
      payload: { text: 'после' },
      local: false,
    });
    const recorder = recordingApi((call) => {
      if (call.method === 'DELETE' && call.path === '/messages/5') {
        return Object.assign(new Error('message already finished'), { status: 409 });
      }
      return { ok: true };
    });
    const sync = emptySync({
      api: recorder.api,
      cache,
      mutations,
      renderList: () => {},
    });
    const result = await sync.tick();
    expect(result.failed).not.toBe(true);
    expect(result.refused).toEqual([
      { seq: 1, kind: 'message.delete', reason: 'message already finished' },
    ]);
    expect(await mutations.count()).toBe(0);
    // A refused delete keeps its local tombstone and the cached row; the
    // accepted edit after it is committed and drops its override.
    expect(cache.deleted).toEqual([]);
    expect(cache.commits).toEqual([
      { method: 'edit', monolog: 1, message: 6, value: 'после' },
    ]);
    expect(recorder.sent().map((call) => call.method + ' ' + call.path)).toEqual([
      'DELETE /messages/5',
      'PATCH /messages/6',
    ]);
  });

  it('sends a change for a locally made monolog under its server id', async () => {
    const cache = fakeCache();
    cache.locals.set(-1, { name: 'Новый', serverId: null });
    const { mutations } = mutationQueue();
    await mutations.put({
      kind: 'monolog.rename',
      target: { monolog: -1 },
      payload: { name: 'Новый' },
      local: true,
    });
    const server = monologApi();
    const sync = emptySync({ api: server.api, cache, mutations, renderList: () => {} });
    const result = await sync.tick();
    expect(result.refused).toEqual([]);
    expect(await cache.serverId(-1)).toBe(77);
    expect(await mutations.count()).toBe(0);
    expect(server.calls.findIndex((call) => call.startsWith('POST /monologues'))).toBeLessThan(
      server.calls.findIndex((call) => call.startsWith('PATCH /monologues/77')),
    );
    expect(server.calls.includes('PATCH /monologues/77')).toBe(true);
    expect(server.calls.some((call) => call.includes('/monologues/-1'))).toBe(false);
    // The accepted rename is written under the server id, not the local one.
    expect(cache.commits).toEqual([{ method: 'rename', monolog: 77, value: 'Новый' }]);
  });

  it('waits for the monolog instead of letting a later change overtake', async () => {
    const cache = fakeCache();
    cache.locals.set(-1, { name: 'Офлайн', serverId: null });
    const { mutations } = mutationQueue();
    await mutations.put({
      kind: 'monolog.rename',
      target: { monolog: -1 },
      payload: { name: 'Офлайн' },
      local: true,
    });
    await mutations.put({
      kind: 'monolog.archive',
      target: { monolog: 77 },
      payload: { archived: true },
      local: false,
    });
    let server = monologApi({ failMonolog: true });
    const sync = emptySync({
      api: (method, path, body) => server.api(method, path, body),
      cache,
      mutations,
      renderList: () => {},
    });
    const first = await sync.tick();
    expect(first.failed).toBe(true);
    expect(await mutations.count()).toBe(2);
    expect(server.calls.some((call) => call.startsWith('PATCH'))).toBe(false);

    server = monologApi();
    const second = await sync.tick();
    expect(second.refused).toEqual([]);
    expect(await mutations.count()).toBe(0);
    const sent = server.calls.filter((call) => !call.startsWith('GET'));
    expect(sent).toEqual([
      'POST /monologues ' + JSON.stringify({ name: 'Офлайн' }),
      'PATCH /monologues/77',
      'POST /monologues/77/archive',
    ]);
    expect(cache.commits).toEqual([
      { method: 'rename', monolog: 77, value: 'Офлайн' },
      { method: 'archive', monolog: 77, value: true },
    ]);
  });

  it('treats a delete the computer already made as accepted', async () => {
    const cache = fakeCache();
    await cache.putMessages(1, [{ id: 5, recorded: 'x', text: 'уже нет' }]);
    const { mutations } = mutationQueue();
    await mutations.put({
      kind: 'message.delete',
      target: { monolog: 1, message: 5 },
      payload: {},
      local: false,
    });
    const recorder = recordingApi((call) => {
      if (call.method === 'DELETE') {
        return Object.assign(new Error('no such message'), { status: 404 });
      }
      return { ok: true };
    });
    const sync = emptySync({ api: recorder.api, cache, mutations, renderList: () => {} });
    const result = await sync.tick();
    // A message that is already gone is the outcome the person asked for.
    expect(result.refused).toEqual([]);
    expect(result.failed).not.toBe(true);
    expect(await mutations.count()).toBe(0);
    expect(cache.deleted).toEqual([{ monolog: 1, id: 5 }]);
  });

  it('refuses a rename or an edit on any 4xx without committing it', async () => {
    const cache = fakeCache();
    const { mutations } = mutationQueue();
    await mutations.put({
      kind: 'monolog.rename',
      target: { monolog: 1 },
      payload: { name: 'Нет' },
      local: false,
    });
    await mutations.put({
      kind: 'message.edit',
      target: { monolog: 1, message: 5 },
      payload: { text: 'нет' },
      local: false,
    });
    const recorder = recordingApi((call) => {
      if (call.method === 'PATCH') {
        return Object.assign(new Error('refused'), { status: 403 });
      }
      return { ok: true };
    });
    const sync = emptySync({ api: recorder.api, cache, mutations, renderList: () => {} });
    const result = await sync.tick();
    expect(result.refused).toEqual([
      { seq: 1, kind: 'monolog.rename', reason: 'refused' },
      { seq: 2, kind: 'message.edit', reason: 'refused' },
    ]);
    expect(await mutations.count()).toBe(0);
    expect(cache.commits).toEqual([]);
  });

  it('keeps a mutation queued when the cache write fails', async () => {
    const cache = fakeCache();
    const { mutations } = mutationQueue();
    await mutations.put({
      kind: 'monolog.rename',
      target: { monolog: 1 },
      payload: { name: 'Раз' },
      local: false,
    });
    let failCommit = true;
    const commitRename = cache.commitRename;
    cache.commitRename = async (monologId, name) => {
      if (failCommit) throw new Error('cache is closed');
      await commitRename(monologId, name);
    };
    const recorder = recordingApi();
    const sync = emptySync({ api: recorder.api, cache, mutations, renderList: () => {} });
    const first = await sync.tick();
    // The acknowledgement is not lost: the entry is still first in line.
    expect(first.failed).toBe(true);
    expect(await mutations.count()).toBe(1);
    expect(cache.commits).toEqual([]);

    failCommit = false;
    const second = await sync.tick();
    expect(second.failed).not.toBe(true);
    expect(await mutations.count()).toBe(0);
    expect(cache.commits).toEqual([{ method: 'rename', monolog: 1, value: 'Раз' }]);
    // The retry re-sent an idempotent rename, which is the price of keeping the
    // acknowledgement instead of dropping it.
    expect(recorder.sent().filter((call) => call.path === '/monologues/1').length).toBe(2);
  });
});
