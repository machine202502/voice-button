// Where the phone's back gesture goes. Run with: node tests/back-test.mjs

import { backTarget, viewState } from '../src/web/js/back.mjs';

let checks = 0;
const failures = [];

function check(condition, message) {
  checks += 1;
  if (condition) {
    console.log('  ok    ' + message);
  } else {
    failures.push(message);
    console.log('  FAIL  ' + message);
  }
}

const monolog = { id: 7, name: 'Работа' };

// ---------------------------------------------------------------- order

check(
  backTarget({ sheets: 1, monolog, archived: false }) === 'sheet',
  'a sheet closes first, even on top of a monolog',
);
check(
  backTarget({ sheets: 2, monolog, archived: true }) === 'sheet',
  'stacked sheets still close one at a time',
);
check(
  backTarget({ sheets: 0, monolog, archived: false }) === 'monolog',
  'inside a monolog back returns to the list',
);
check(
  backTarget({ sheets: 0, monolog, archived: true }) === 'monolog',
  'the monolog beats the archive when both are set',
);
check(
  backTarget({ sheets: 0, monolog: null, archived: true }) === 'archive',
  'in the archive back returns to the open list',
);
check(
  backTarget({ sheets: 0, monolog: null, archived: false }) === 'none',
  'on the list nothing is ours, so the phone may leave the app',
);

// ---------------------------------------------------------------- entries

check(
  viewState({ monolog }).view === 'monolog' && viewState({ monolog }).id === 7,
  'a monolog pushes an entry with its id',
);
check(
  viewState({ archived: true }).view === 'archive',
  'the archive pushes its own entry',
);
check(
  viewState({}).view === 'list',
  'the list is the resting entry',
);

console.log('');
if (failures.length > 0) {
  console.log('failed:');
  for (const failure of failures) console.log('  - ' + failure);
  process.exit(1);
}
console.log(`${checks}/${checks} checks passed`);
