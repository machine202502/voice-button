// Tests for the phone's single sync loop, run without a browser.
//
//   node tests/sync-test.mjs
//
// The loop is where the phone used to hang, so every case that used to break is
// written down here: a flush that fails, a flush that is asked for while one is
// running, a take that was never closed, an acknowledged chunk.

import { createSync } from '../src/web/js/sync.mjs';
import { createCache } from '../src/web/js/cache.mjs';

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

/** An in-memory outbox with the same shape the page uses. */
function fakeOutbox(items = []) {
  const state = new Map();
  for (const item of items) state.set(item.localId, { ...item, chunks: [...(item.chunks || [])] });
  return {
    state,
    async list() {
      return [...state.values()].map((item) => ({ ...item }));
    },
    async put(item) {
      state.set(item.localId, { ...item, chunks: state.get(item.localId)?.chunks || [] });
    },
    async remove(localId) {
      state.delete(localId);
    },
    async chunks(localId) {
      return state.get(localId)?.chunks || [];
    },
    async dropChunk(localId, seq) {
      const item = state.get(localId);
      if (item) item.chunks = item.chunks.filter((chunk) => chunk.seq !== seq);
    },
  };
}

/** Records every call the loop makes and answers like the computer would. */
function fakeApi({ fail = false, messages = [] } = {}) {
  const calls = [];
  let nextId = 100;
  return {
    calls,
    async call(method, path, body) {
      if (fail) throw new Error('offline');
      calls.push(method + ' ' + path);
      if (method === 'POST' && path === '/messages') return { id: nextId++ };
      if (method === 'GET') return { messages };
      return { ok: true };
    },
  };
}

const silent = () => {};

/** A key/value bag with the storage shape. */
function bag() {
  const map = new Map();
  return {
    map,
    async get(key) {
      return map.get(key);
    },
    async put(key, value) {
      map.set(key, value);
    },
    async update(key, change) {
      const next = change(map.get(key));
      if (next !== undefined) map.set(key, next);
    },
    async remove(key) {
      map.delete(key);
    },
    async keys() {
      return [...map.keys()];
    },
  };
}

function fakeCache() {
  return createCache({
    monologs: bag(),
    messages: bag(),
    now: () => '2026-11-12T15:00:00+05:00',
  });
}

/** A computer with monologs and messages; `plan` can make one call fail. */
function monologApi({ failMonolog = false, failAll = false } = {}) {
  const calls = [];
  let nextMessage = 100;
  return {
    calls,
    async call(method, path, body) {
      if (failAll) throw new Error('offline');
      if (method === 'POST' && path === '/monologues') {
        if (failMonolog) throw new Error('offline');
        calls.push('POST /monologues ' + JSON.stringify(body));
        return { ok: true, monolog: { id: 77, name: body.name } };
      }
      calls.push(method + ' ' + path);
      if (method === 'POST' && path === '/messages') return { id: nextMessage++ };
      if (method === 'GET' && path === '/monologues') {
        return { monologs: [{ id: 77, name: 'Новый', count: 1 }] };
      }
      if (method === 'GET' && path.startsWith('/monologues/')) return { messages: [] };
      return { ok: true };
    },
  };
}

async function testHappyPath() {
  console.log('a take goes out and is closed');
  const outbox = fakeOutbox([
    {
      localId: 'a',
      monolog: 1,
      recorded: '2026-11-12T15:00:00+05:00',
      remote: 7,
      closed: true,
      chunks: [
        { seq: 0, bytes: new Uint8Array([1, 0]) },
        { seq: 1, bytes: new Uint8Array([2, 0]) },
      ],
    },
  ]);
  const api = fakeApi();
  let drawn = null;
  const sync = createSync({
    api: (method, path, body) => api.call(method, path, body),
    outbox,
    render: async (messages) => {
      drawn = messages;
    },
    log: silent,
  });
  sync.show(1);
  const result = await sync.tick();
  check(result.sent === 1, 'one take was sent');
  check(
    api.calls.join('|') ===
      'POST /messages/7/audio?seq=0|POST /messages/7/audio?seq=1|POST /messages/7/finish|GET /monologues/1/messages',
    'audio, then finish, then the fetch: ' + api.calls.join('|'),
  );
  check(outbox.state.size === 0, 'the take left the outbox only after the computer agreed');
  check(Array.isArray(drawn), 'the page was drawn once');
}

async function testOpenTakeStays() {
  console.log('a take that is still recording is not closed');
  const outbox = fakeOutbox([
    { localId: 'b', monolog: 1, recorded: 'x', remote: 8, closed: false, chunks: [] },
  ]);
  const api = fakeApi();
  const sync = createSync({
    api: (m, p, b) => api.call(m, p, b),
    outbox,
    render: async () => {},
    log: silent,
  });
  sync.show(1);
  await sync.tick();
  check(
    !api.calls.some((call) => call.includes('finish')),
    'no finish for a take the user has not stopped',
  );
  check(outbox.state.size === 1, 'the take is still in the outbox');
}

async function testNewTakeGetsAnId() {
  console.log('a take with no id gets one before its audio');
  const outbox = fakeOutbox([
    { localId: 'c', monolog: 1, recorded: 'x', remote: null, closed: true, chunks: [] },
  ]);
  const api = fakeApi();
  const sync = createSync({
    api: (m, p, b) => api.call(m, p, b),
    outbox,
    render: async () => {},
    log: silent,
  });
  sync.show(1);
  await sync.tick();
  check(api.calls[0] === 'POST /messages', 'the message is created first');
  check(
    api.calls.includes('POST /messages/100/finish'),
    'the new id is used for the rest of the take: ' + api.calls.join('|'),
  );
  check(outbox.state.size === 0, 'and the take leaves once the computer agreed');
}

async function testFailuresDoNotStick() {
  console.log('a failed flush leaves nothing stuck');
  const outbox = fakeOutbox([
    { localId: 'd', monolog: 1, recorded: 'x', remote: 9, closed: true, chunks: [{ seq: 0, bytes: new Uint8Array([1, 0]) }] },
  ]);
  const broken = fakeApi({ fail: true });
  let working = fakeApi();
  const sync = createSync({
    api: (m, p, b) => working.call(m, p, b),
    outbox,
    render: async () => {},
    log: silent,
  });
  sync.show(1);
  // Point the same loop at a broken connection for one tick.
  const original = working.call;
  working.call = (m, p, b) => broken.call(m, p, b);
  const first = await sync.tick();
  check(first.failed === true, 'the failure is reported');
  check(outbox.state.size === 1, 'the take is kept');
  working.call = original;
  const second = await sync.tick();
  check(second.failed !== true, 'the next tick is not blocked by the previous failure');
  check(outbox.state.size === 0, 'and the take finally goes out');
}

async function testFailedFlushStillShowsTheTake() {
  console.log('a take recorded while the computer is away is shown as waiting');
  const outbox = fakeOutbox([
    { localId: 'e', monolog: 1, recorded: 'x', remote: null, closed: true, chunks: [] },
  ]);
  const broken = fakeApi({ fail: true });
  let drawn = null;
  const sync = createSync({
    api: (m, p, b) => broken.call(m, p, b),
    outbox,
    render: async (messages, pending) => {
      drawn = { messages, pending };
    },
    log: silent,
  });
  sync.show(1);
  const result = await sync.tick();
  check(result.failed === true, 'the failure is reported');
  check(drawn !== null, 'the page is drawn anyway');
  check(
    drawn && drawn.pending.length === 1 && drawn.pending[0].localId === 'e',
    'the waiting take is on the screen, not only in the outbox',
  );
}

async function testSwitchingMonologsForgetsTheOldList() {
  console.log('a monolog opened while the computer is away shows nothing of the other');
  // A take still waiting makes the flush fail first, which is the path that
  // draws without asking the computer for the list.
  const outbox = fakeOutbox([
    {
      localId: 'g',
      monolog: 2,
      recorded: 'x',
      remote: null,
      closed: false,
      chunks: [{ seq: 0, bytes: new Uint8Array([1, 0]) }],
    },
  ]);
  let online = true;
  const api = {
    async call(method, path) {
      if (!online) throw new Error('offline');
      if (method === 'POST' && path === '/messages') return { id: 1 };
      return method === 'GET' ? { messages: [{ id: 1, stage: 'done', text: 'старое' }] } : { ok: true };
    },
  };
  let drawn = null;
  const sync = createSync({
    api: (m, p, b) => api.call(m, p, b),
    outbox,
    render: async (messages) => {
      drawn = messages;
    },
    log: silent,
  });
  sync.show(1);
  await sync.tick();
  check(drawn && drawn.length === 1, 'the first monolog is drawn');
  // The recorder is still going: another chunk waits, and now the link is gone.
  outbox.state.get('g').chunks.push({ seq: 1, bytes: new Uint8Array([2, 0]) });
  online = false;
  sync.show(2);
  const result = await sync.tick();
  check(result.failed === true, 'the computer is away');
  check(Array.isArray(drawn) && drawn.length === 0, 'the new one is empty, not the old list');
}

async function testNoOverlap() {
  console.log('ticks do not overlap');
  let inFlight = 0;
  let maxInFlight = 0;
  const outbox = fakeOutbox();
  const api = {
    async call() {
      inFlight += 1;
      maxInFlight = Math.max(maxInFlight, inFlight);
      await new Promise((resolve) => setTimeout(resolve, 20));
      inFlight -= 1;
      return { messages: [] };
    },
  };
  const sync = createSync({
    api: (m, p, b) => api.call(m, p, b),
    outbox,
    render: async () => {},
    log: silent,
  });
  sync.show(1);
  const results = await Promise.all([sync.tick(), sync.tick(), sync.tick()]);
  check(maxInFlight === 1, 'never more than one tick at a time: ' + maxInFlight);
  check(results.filter((result) => result.skipped).length === 2, 'the extra calls are skipped');
}

async function testRenderOnlyWhenShowing() {
  console.log('no fetch while no monolog is open');
  const outbox = fakeOutbox();
  const api = fakeApi();
  const sync = createSync({
    api: (m, p, b) => api.call(m, p, b),
    outbox,
    render: async () => {},
    log: silent,
  });
  await sync.tick();
  check(api.calls.length === 0, 'the list of monologs is not fetched behind our back');
}

async function testLocalMonologIsCreatedBeforeItsTake() {
  console.log('a monolog made on the phone is created before its take is sent');
  const cache = fakeCache();
  const local = await cache.allocate('Новый');
  const outbox = fakeOutbox([
    {
      localId: 'h',
      monolog: local.id,
      recorded: '2026-11-12T15:00:00+05:00',
      remote: null,
      closed: false,
      chunks: [{ seq: 0, bytes: new Uint8Array([1, 0]) }],
    },
  ]);
  const api = monologApi();
  const drawn = [];
  const mapped = [];
  const sync = createSync({
    api: (m, p, b) => api.call(m, p, b),
    outbox,
    cache,
    mapped: (localId, serverId) => mapped.push([localId, serverId]),
    render: async (messages) => {
      drawn.push(messages);
    },
    renderList: async () => {},
    log: silent,
  });
  sync.show(local.id);
  const result = await sync.tick();
  check(result.failed !== true, 'the tick succeeds once the computer answers');
  check(
    api.calls.findIndex((call) => call.startsWith('POST /monologues')) <
      api.calls.findIndex((call) => call.startsWith('POST /messages')),
    'the monolog is created first: ' + api.calls.join('|'),
  );
  check(
    api.calls.includes('POST /messages'),
    'the take is sent after the monolog exists',
  );
  check((await cache.serverId(local.id)) === 77, 'the mapping local -> server is stored');
  check(
    outbox.state.get('h') && outbox.state.get('h').monolog === 77,
    'the stored take uses the mapped id, not -1',
  );
  check(
    outbox.state.get('h') && outbox.state.get('h').remote === 100,
    'and carries the message id the computer gave it',
  );
  check(
    mapped.length === 1 && mapped[0][0] === -1 && mapped[0][1] === 77,
    'the open page is told its monolog got a server id',
  );
  check(
    Array.isArray(drawn[drawn.length - 1]) && drawn[drawn.length - 1].length === 0,
    'the messages of the mapped server monolog are fetched, not of -1',
  );
  check(
    api.calls.includes('GET /monologues'),
    'the list is refreshed even while the monolog page is open',
  );
  check(
    (await cache.serverMonologs(false)).length === 1,
    'and the refreshed list is cached',
  );

  // Closing the take later completes it under the same server monolog.
  outbox.state.get('h').closed = true;
  const finished = await sync.tick();
  check(finished.failed !== true, 'the second tick finishes the take');
  check(outbox.state.size === 0, 'and the take leaves after the computer agreed');
}

async function testMappingFailureKeepsTheTake() {
  console.log('a take is never lost while its monolog has no server id');
  const cache = fakeCache();
  const local = await cache.allocate('Ждёт');
  const outbox = fakeOutbox([
    {
      localId: 'i',
      monolog: local.id,
      recorded: 'x',
      remote: null,
      closed: true,
      chunks: [{ seq: 0, bytes: new Uint8Array([1, 0]) }],
    },
  ]);
  const api = monologApi({ failMonolog: true });
  let drawn = null;
  const sync = createSync({
    api: (m, p, b) => api.call(m, p, b),
    outbox,
    cache,
    render: async (messages, pending) => {
      drawn = { messages, pending };
    },
    log: silent,
  });
  sync.show(local.id);
  const first = await sync.tick();
  check(first.failed === true, 'the failed monolog creation is reported');
  check(outbox.state.size === 1, 'the take stays in the outbox');
  check(outbox.state.get('i').monolog === local.id, 'with its local monolog id');
  check(
    !api.calls.some((call) => call.startsWith('POST /messages')),
    'no take is sent into a monolog the computer does not have',
  );
  check(drawn && drawn.pending.length === 1, 'the waiting take is still on the screen');

  // The computer comes back: the same tick retries the monolog and the take.
  api.call = monologApi().call;
  const second = await sync.tick();
  check(second.failed !== true, 'the next tick gets through');
  check(outbox.state.size === 0, 'and the take finally leaves');
  check((await cache.serverId(local.id)) === 77, 'the mapping is stored on the retry');
}

async function testOfflineDrawsTheCachedMessages() {
  console.log('a monolog opened without the computer shows its cached messages');
  const cache = fakeCache();
  await cache.putMessages(1, [{ id: 5, stage: 'done', text: 'старое', recorded: 'x' }]);
  const outbox = fakeOutbox([
    { localId: 'j', monolog: 1, recorded: 'x', remote: null, closed: true, chunks: [] },
  ]);
  const offline = monologApi({ failAll: true });
  let drawn = null;
  const sync = createSync({
    api: (m, p, b) => offline.call(m, p, b),
    outbox,
    cache,
    render: async (messages, pending) => {
      drawn = { messages, pending };
    },
    log: silent,
  });
  sync.show(1);
  const result = await sync.tick();
  check(result.failed === true, 'the dead connection is reported');
  check(
    drawn && drawn.messages.length === 1 && drawn.messages[0].text === 'старое',
    'the cached messages are drawn',
  );
  check(drawn && drawn.pending.length === 1, 'together with the waiting take');
}

async function testListIsFetchedAndCached() {
  console.log('the list is fetched, cached and drawn while the list is open');
  const cache = fakeCache();
  const outbox = fakeOutbox();
  const api = monologApi();
  let drawn = null;
  const sync = createSync({
    api: (m, p, b) => api.call(m, p, b),
    outbox,
    cache,
    render: async () => {},
    renderList: async (list) => {
      drawn = list;
    },
    log: silent,
  });
  const result = await sync.tick();
  check(result.monologs === 1, 'the list came back');
  check(drawn && drawn.length === 1 && drawn[0].name === 'Новый', 'the list is drawn');
  check((await cache.serverMonologs(false)).length === 1, 'and stored in the cache');
}

async function testARefusedTakeContinuesInANewMessage() {
  console.log('a take the computer already finished continues in a new message');
  const outbox = fakeOutbox([
    {
      localId: 'refused',
      monolog: 1,
      recorded: '2026-11-13T03:00:00+05:00',
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
      recorded: '2026-11-13T03:00:01+05:00',
      remote: 6,
      closed: true,
      chunks: [{ seq: 0, bytes: new Uint8Array([5, 6]) }],
    },
  ]);
  const calls = [];
  const api = async (method, path) => {
    calls.push(method + ' ' + path);
    if (path.startsWith('/messages/5/audio')) {
      const refusal = new Error('this message is not receiving audio');
      refusal.status = 409;
      throw refusal;
    }
    if (method === 'POST' && path === '/messages') return { message: { id: 100 } };
    if (method === 'GET') return { monologs: [] };
    return { ok: true };
  };
  const sync = createSync({
    api,
    outbox,
    cache: null,
    render: async () => {},
    renderList: async () => {},
    log: silent,
  });
  const result = await sync.tick();
  check(result.failed !== true, 'the refused take is not a failed tick');
  check(result.offline !== true, 'and it is not reported as a lost connection');
  check(calls.includes('POST /messages'), 'a new message is opened for the rest');
  check(
    calls.includes('POST /messages/100/audio?seq=0')
      && calls.includes('POST /messages/100/audio?seq=1'),
    'the remaining chunks, including the refused one, go to the new message',
  );
  check(calls.includes('POST /messages/100/finish'), 'and the new message is finished');
  check(!(await outbox.list()).some((take) => take.localId === 'refused'),
    'the whole take leaves only after the rest was delivered');
  check(!(await outbox.list()).some((take) => take.localId === 'good'),
    'the take after it is still sent');
}

async function testPendingRenameIsSentOnce() {
  console.log('a rename waits for the connection and is sent once');
  const cache = fakeCache();
  await cache.putServerMonologs([{ id: 4, name: 'Старое' }], false);
  await cache.rename(4, 'Новое');
  let patched = null;
  let serverName = 'Старое';
  const api = async (method, path, body) => {
    if (method === 'PATCH') {
      patched = { path, body };
      serverName = body.name;
    }
    return { ok: true, monologs: [{ id: 4, name: serverName }] };
  };
  const sync = createSync({
    api,
    outbox: fakeOutbox(),
    cache,
    render: async () => {},
    renderList: async () => {},
    log: silent,
  });
  await sync.tick();
  check(
    patched && patched.path === '/monologues/4' && patched.body.name === 'Новое',
    'the pending name reached the computer',
  );
  check((await cache.dirtyRenames()).length === 0, 'and the queue is empty again');
  check(
    (await cache.serverMonologs(false))[0].name === 'Новое',
    'the new name survives the list refresh',
  );
}

async function testListCarriesTheViewItWasFetchedFor() {
  console.log('a list knows whether it was the archive');
  const cache = fakeCache();
  const outbox = fakeOutbox();
  let seen = null;
  const sync = createSync({
    api: async () => ({ monologs: [] }),
    outbox,
    cache,
    render: async () => {},
    renderList: async (list, archivedView) => {
      seen = archivedView;
    },
    log: silent,
  });
  sync.archive(true);
  await sync.tick();
  check(seen === true, 'the archive list is marked as the archive');
}

async function testStoredMappingReachesAnOldTake() {
  console.log('a mapping stored by an earlier tick still reaches the take');
  const cache = fakeCache();
  const local = await cache.allocate('Новый');
  // The monolog reached the computer on an earlier tick, but the take kept the
  // negative id: the next tick must not wait for a monolog creation that will
  // never happen again.
  await cache.mapLocal(local.id, 77);
  const outbox = fakeOutbox([
    { localId: 'k', monolog: local.id, recorded: 'x', remote: null, closed: true, chunks: [] },
  ]);
  const api = monologApi();
  const sync = createSync({
    api: (m, p, b) => api.call(m, p, b),
    outbox,
    cache,
    render: async () => {},
    log: silent,
  });
  sync.show(local.id);
  const result = await sync.tick();
  check(result.failed !== true, 'the tick gets through');
  check(
    !api.calls.some((call) => call.startsWith('POST /monologues')),
    'the monolog is not created a second time',
  );
  check(outbox.state.size === 0, 'the take is sent under the stored mapping');
  check(
    api.calls.includes('GET /monologues/77/messages'),
    'and the messages are fetched from the server monolog',
  );
}

const suites = [
  testHappyPath,
  testOpenTakeStays,
  testNewTakeGetsAnId,
  testFailuresDoNotStick,
  testFailedFlushStillShowsTheTake,
  testSwitchingMonologsForgetsTheOldList,
  testNoOverlap,
  testRenderOnlyWhenShowing,
  testLocalMonologIsCreatedBeforeItsTake,
  testStoredMappingReachesAnOldTake,
  testMappingFailureKeepsTheTake,
  testOfflineDrawsTheCachedMessages,
  testListIsFetchedAndCached,
  testListCarriesTheViewItWasFetchedFor,
  testPendingRenameIsSentOnce,
  testARefusedTakeContinuesInANewMessage,
];

for (const suite of suites) {
  await suite();
}

console.log('');
console.log(`${checks - failures}/${checks} checks passed`);
process.exit(failures === 0 ? 0 : 1);
