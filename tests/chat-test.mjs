// Tests for the phone's message list.
//
//   node tests/chat-test.mjs
//
// The list is the part the user reads, so order and dividers are checked
// against the cases that happen in real life: a late upload from an offline
// phone, two takes in the same minute, and a message with a broken timestamp.

import { sortAscending, dayKey, buildFeed } from '../src/web/js/chat.mjs';

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

const take = (id, recorded) => ({ id, recorded, stage: 'done', text: 'т' + id });

console.log('oldest first, whatever order the computer sent');
const shuffled = [
  take(3, '2026-11-12T15:10:00+05:00'),
  take(1, '2026-11-11T09:00:00+05:00'),
  take(2, '2026-11-12T15:08:00+05:00'),
];
check(
  sortAscending(shuffled).map((message) => message.id).join(',') === '1,2,3',
  'sorted by the phone clock: ' + sortAscending(shuffled).map((m) => m.id).join(','),
);
check(shuffled[0].id === 3, 'the input list is not modified in place');

console.log('a late upload sits where it was spoken');
const late = [take(1, '2026-11-12T10:00:00+05:00'), take(2, '2026-11-12T09:00:00+05:00')];
check(
  sortAscending(late).map((message) => message.id).join(',') === '2,1',
  'the message spoken earlier is first, even though it arrived later',
);

console.log('two takes in the same minute keep their order');
const same = [take(7, '2026-11-12T10:00:00+05:00'), take(8, '2026-11-12T10:00:00+05:00')];
check(
  sortAscending(same).map((message) => message.id).join(',') === '7,8',
  'equal timestamps do not swap places',
);

console.log('one divider per day');
const feed = buildFeed([
  take(1, '2026-11-11T09:00:00+05:00'),
  take(2, '2026-11-12T10:00:00+05:00'),
  take(3, '2026-11-12T11:00:00+05:00'),
]);
const days = feed.filter((row) => row.kind === 'day').map((row) => row.key);
check(days.join(',') === '2026-11-11,2026-11-12', 'days are grouped: ' + days.join(','));
check(feed.length === 5, 'three takes and two dividers: ' + feed.length);
check(feed[0].kind === 'day', 'the list starts with its day');
check(feed[1].kind === 'message' && feed[1].message.id === 1, 'then the first take');

console.log('nothing is dropped');
const broken = buildFeed([take(1, '2026-11-12T10:00:00+05:00'), { id: 2, recorded: '', stage: 'silent' }]);
check(
  broken.filter((row) => row.kind === 'message').length === 2,
  'a take with a broken timestamp is still shown',
);
check(dayKey('') === '', 'an empty stamp has no day');
check(dayKey('2026-11-12T10:00:00+05:00') === '2026-11-12', 'the day is taken from the stamp');

console.log('an empty list stays empty');
check(buildFeed([]).length === 0, 'no rows for no messages');

console.log('equal timestamps keep a stable order');
const tied = buildFeed([
  { id: 2, recorded: '2026-11-12T10:00:00+05:00', text: 'b' },
  { id: 1, recorded: '2026-11-12T10:00:00+05:00', text: 'a' },
]);
const tiedIds = tied.filter((row) => row.kind === 'message').map((row) => row.message.id);
check(tiedIds.join(',') === '1,2', 'same stamp: rows are ordered by id, not by luck');

console.log('');
console.log(`${checks - failures}/${checks} checks passed`);
process.exit(failures === 0 ? 0 : 1);
