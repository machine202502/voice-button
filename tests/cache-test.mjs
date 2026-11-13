// Tests for the phone's offline cache and its pure decisions.
//
//   node tests/cache-test.mjs
//
// No browser: the bags are maps with the same shape storage.mjs hands the page.
// The cases are the ones a dead connection used to turn into a dead end: which
// monologs the list shows, how a local id is chosen, and when boot may keep the
// page instead of the pairing screen.

import { bootOutcome, createCache, mergeMonologs, nextLocalId } from '../src/web/js/cache.mjs';

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

/** One key/value bag with the interface storage.mjs exposes. */
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

function compareMessages(left, right) {
  if (left.recorded < right.recorded) return -1;
  if (left.recorded > right.recorded) return 1;
  return left.id - right.id;
}

/** The paged message store, with the same contract storage.mjs implements. */
function messageStore() {
  const rows = new Map();
  const key = (message) => message.monolog + '|' + message.recorded + '|' + message.id;
  return {
    rows,
    async put(list) {
      for (const message of list) rows.set(key(message), { ...message });
      return list.length;
    },
    async page(monolog, { before = null, after = null, limit = 50 } = {}) {
      const all = [...rows.values()]
        .filter((message) => message.monolog === monolog)
        .sort(compareMessages);
      if (before) {
        const older = all.filter((message) => compareMessages(message, before) < 0);
        const chunk = older.slice(-(limit + 1));
        return { messages: chunk.slice(-limit), hasMore: chunk.length > limit };
      }
      if (after) {
        const newer = all.filter((message) => compareMessages(message, after) > 0);
        return { messages: newer.slice(0, limit), hasMore: newer.length > limit };
      }
      const chunk = all.slice(-(limit + 1));
      return { messages: chunk.slice(-limit), hasMore: chunk.length > limit };
    },
    async count(monolog) {
      return [...rows.values()].filter((message) => message.monolog === monolog).length;
    },
    async remove(monolog, recorded, id) {
      rows.delete(monolog + '|' + recorded + '|' + id);
    },
    async any() {
      return rows.size > 0;
    },
  };
}

function cacheWith(monologs = bag(), messages = messageStore()) {
  return createCache({
    monologs,
    messages,
    now: () => '2026-11-12T15:00:00+05:00',
  });
}

console.log('local ids are negative and go down');
{
  check(nextLocalId([]) === -1, 'the first local monolog is -1');
  check(nextLocalId([-1]) === -2, 'the next one is one less: -2');
  check(nextLocalId([4, -1, 'x', -3]) === -4, 'the lowest existing id wins over server ids');
  check(nextLocalId(['a', 'b']) === -1, 'non-numeric keys are ignored');
}

console.log('the list is cached monologs plus unsent local ones');
{
  const server = [
    { id: 7, name: 'Работа', count: 3 },
    { id: 5, name: 'Дом', count: 1 },
  ];
  const local = [
    { id: -1, localId: -1, name: 'В дороге', serverId: null },
  ];
  const merged = mergeMonologs(server, local);
  check(
    merged.map((item) => item.id).join(',') === '7,5,-1',
    'server monologs stay first, the unsent one is added: ' + merged.map((item) => item.id),
  );
  check(
    merged[2].local === true && merged[0].local === false,
    'only the unsent monolog is marked local',
  );
}

console.log('a mapped monolog is not shown twice');
{
  const server = [{ id: 9, name: 'Работа', count: 2 }];
  const mapped = [{ id: -1, localId: -1, name: 'Работа', serverId: 9 }];
  check(
    mergeMonologs(server, mapped).length === 1,
    'the server entry wins over the mapped local one',
  );
  const notListed = mergeMonologs(server, [{ id: -2, localId: -2, name: 'Новый', serverId: 11 }]);
  check(
    notListed.map((item) => item.id).join(',') === '11,9',
    'a mapped local monolog the server has not listed yet still appears under its server id',
  );
  check(
    notListed[0].id === 11 && notListed[0].local === false,
    'it uses the server id and is no longer "waiting"',
  );
}

console.log('allocating a local monolog stores it and goes down');
{
  const monologs = bag();
  const cache = cacheWith(monologs);
  const first = await cache.allocate('Первый');
  const second = await cache.allocate('Второй');
  check(first.id === -1 && second.id === -2, 'two monologs get -1 then -2');
  check(
    first.name === 'Первый' && first.created === '2026-11-12T15:00:00+05:00',
    'the name and the phone clock are stored',
  );
  const local = await cache.localMonologs();
  check(local.length === 2, 'both are listed as local');
  await cache.mapLocal(-1, 42);
  check((await cache.serverId(-1)) === 42, 'the mapping is remembered');
  check((await cache.serverId(-2)) === null, 'an unsent one has no server id');
}

console.log('server views are cached separately from local monologs');
{
  const monologs = bag();
  const cache = cacheWith(monologs);
  await cache.allocate('Ждёт');
  await cache.putServerMonologs([{ id: 1, name: 'Работа', count: 2 }], false);
  const open = await cache.monologPage(false, 0, 100);
  check(open.monologs.some((item) => item.id === 1), 'the open list is stored');
  await cache.putServerMonologs([{ id: 1, name: 'Работа', count: 2, archived: true }], true);
  check(
    !(await cache.monologPage(false, 0, 100)).monologs.some((item) => item.id === 1),
    'an archived monolog leaves the open view',
  );
  check(
    (await cache.monologPage(true, 0, 100)).monologs.some((item) => item.id === 1),
    'and appears in the archive view',
  );
  await cache.putServerMonologs([{ id: 3, name: 'Ещё', count: 0 }, { id: 4, name: 'Старое' }], false);
  await cache.putServerMonologs([{ id: 3, name: 'Ещё', count: 0 }], false, { complete: true });
  check(
    (await cache.monologPage(false, 0, 100)).monologs
      .filter((item) => item.id > 0)
      .map((item) => item.id)
      .join(',') === '3',
    'a complete first page drops what the computer no longer lists',
  );
  check((await cache.localMonologs()).length === 1, 'the local monolog is untouched by a list refresh');
}

console.log('the monolog list is read one page at a time');
{
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
  check(
    first.monologs.map((item) => item.id).join(',') === '-1,3',
    'the first page is newest first and includes the local one: ' +
      first.monologs.map((item) => item.id),
  );
  check(first.total === 4 && first.hasMore === true, 'the page knows its total and that more exist');
  const second = await cache.monologPage(false, 2, 2);
  check(
    second.monologs.map((item) => item.id).join(',') === '2,1',
    'the next page continues the same order: ' + second.monologs.map((item) => item.id),
  );
  check(second.hasMore === false, 'and the last page says so');
}

console.log('messages are cached per monolog and read in pages');
{
  const cache = cacheWith();
  const recorded = '2026-11-12T10:00:00+05:00';
  const later = '2026-11-12T11:00:00+05:00';
  await cache.putMessages(4, [
    { id: 1, recorded, text: 'привет' },
    { id: 2, recorded: later, text: 'ещё' },
  ]);
  const latest = await cache.messagePage(4, { limit: 1 });
  check(
    latest.messages.length === 1 && latest.messages[0].text === 'ещё',
    'the newest page comes first',
  );
  const older = await cache.messagePage(4, {
    before: { recorded: later, id: 2 },
    limit: 5,
  });
  check(
    older.messages.length === 1 && older.messages[0].text === 'привет',
    'before reads older messages',
  );
  check((await cache.messageCount(4)) === 2, 'count sees both');
  check(
    (await cache.messagePage(5, { limit: 5 })).messages.length === 0,
    'a monolog with no cache is empty, not an error',
  );
  check((await cache.hasData()) === true, 'cached messages count as data for the boot decision');
  await cache.deleteMessage(4, { recorded: later, id: 2 });
  check((await cache.messageCount(4)) === 1, 'deleting one message removes only that one');
}

console.log('a paired phone always keeps the page');
{
  check(
    bootOutcome({ hasToken: true, offline: true, hasCache: true }) === 'app',
    'a token and a cache: the page stays usable offline',
  );
  check(
    bootOutcome({ hasToken: true, offline: true, hasCache: false }) === 'app',
    'no cache: there is nothing to show',
  );
  check(
    bootOutcome({ hasToken: false, offline: true, hasCache: true }) === 'pair',
    'no token: pairing is still required',
  );
  check(
    bootOutcome({ hasToken: true, offline: false, hasCache: false }) === 'app',
    'a reachable computer always boots the page',
  );
}

console.log('a rename never waits for the network');
{
  const monologs = bag();
  const cache = cacheWith(monologs);
  const local = await cache.allocate('Старое');
  check(await cache.rename(local.id, 'Новое'), 'a local monolog can be renamed at once');
  const localNow = (await cache.localMonologs())[0];
  check(localNow.name === 'Новое', 'the local name changed');
  check((await cache.dirtyRenames()).length === 0, 'a local monolog needs no PATCH');

  await cache.putServerMonologs([{ id: 3, name: 'Серверное' }], false);
  check(await cache.rename(3, 'Правка'), 'a known monolog is renamed locally too');
  check(
    (await cache.monologPage(false, 0, 100)).monologs.find((item) => item.id === 3).name ===
      'Правка',
    'the open list shows the new name before the computer answers',
  );
  const waiting = await cache.dirtyRenames();
  check(waiting.length === 1 && waiting[0].id === 3, 'and the change is queued for it');

  // A list fetched after the rename must not undo it.
  await cache.putServerMonologs([{ id: 3, name: 'Серверное' }], false);
  check(
    (await cache.monologPage(false, 0, 100)).monologs.find((item) => item.id === 3).name ===
      'Правка',
    'an old server name does not overwrite the pending rename',
  );
}

console.log('archiving moves a monolog between the views at once');
{
  const monologs = bag();
  const cache = cacheWith(monologs);
  await cache.putServerMonologs([{ id: 5, name: 'Работа' }], false);
  check((await cache.monologPage(false, 0, 100)).total === 1, 'the open list has it');
  check(await cache.setArchived(5, true), 'the cache found the record');
  check((await cache.monologPage(false, 0, 100)).total === 0, 'and the open list is empty at once');
  check((await cache.monologPage(true, 0, 100)).total === 1, 'the archive shows it');
  await cache.setArchived(5, false);
  check((await cache.monologPage(false, 0, 100)).total === 1, 'unarchiving brings it back');
}

console.log('the monolog list has one order');
const ordered = mergeMonologs(
  [
    { id: 1, name: 'old', created: '2026-11-12T10:00:00Z' },
    { id: 2, name: 'new', created: '2026-11-12T11:00:00Z' },
  ],
  [{ localId: -1, id: -1, name: 'local', created: '2026-11-12T12:00:00Z' }],
);
check(
  ordered.map((monolog) => monolog.id).join(',') === '-1,2,1',
  'newest first, and a waiting local monolog is where its time says',
);

console.log('');
console.log(`${checks - failures}/${checks} checks passed`);
process.exit(failures === 0 ? 0 : 1);
