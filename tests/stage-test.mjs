// Tests for the phone's stage table, run without a browser.
//
//   node tests/stage-test.mjs
//
// The table decides what the user reads and which buttons exist, so every stage
// is checked: its own word, whether it keeps the page polling, and what may be
// done with it.

import { STAGES, describe, keepsPolling, textOf } from '../src/web/js/stage.mjs';

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

console.log('every stage has its own word');
const labels = STAGES.map((stage) => describe(stage).label);
check(new Set(labels).size === STAGES.length, 'no two stages share a word: ' + labels.join(', '));

console.log('stages that are finished do not keep the page polling');
for (const stage of ['done', 'silent', 'no_model', 'failed', 'cancelled']) {
  check(describe(stage).busy === false, stage + ' is finished');
}
for (const stage of ['draft', 'uploading', 'queued', 'transcribing']) {
  check(describe(stage).busy === true, stage + ' is still moving');
}
check(keepsPolling(['done', 'transcribing']) === true, 'one live stage is enough to poll');
check(keepsPolling(['done', 'silent']) === false, 'nothing live means no polling');

console.log('buttons match the stage');
const edit = describe('done').actions;
check(edit.includes('copy') && edit.includes('edit') && edit.includes('delete'), 'a finished take can be copied, edited, deleted');
for (const stage of ['silent', 'no_model', 'failed']) {
  const actions = describe(stage).actions;
  check(actions.includes('retry'), stage + ' can be retried');
  check(actions.includes('delete'), stage + ' can be deleted');
  check(!actions.includes('edit'), stage + ' has nothing to edit');
}
for (const stage of ['draft', 'uploading', 'queued', 'transcribing']) {
  check(
    describe(stage).actions.join(',') === 'delete',
    stage + ' can only be thrown away',
  );
}
check(describe('cancelled').actions.length === 0, 'a cancelled take offers nothing');

console.log('tones tell the truth');
check(describe('silent').tone === 'muted', 'silence is not an error');
check(describe('no_model').tone === 'warning', 'a missing model is a warning');
check(describe('failed').tone === 'warning', 'a broken engine is a warning');
check(describe('done').tone === 'done', 'a finished take is quiet');

console.log('an unknown stage is treated as a failure');
const stranger = describe('something-new');
check(stranger.stage === 'failed', 'the page does not pretend');
check(stranger.actions.includes('retry'), 'and offers a retry');

console.log('text of a card');
check(textOf({ stage: 'done', text: 'привет' }) === 'привет', 'the text is shown');
check(textOf({ stage: 'transcribing', text: 'пол' }) === 'пол', 'a growing text is shown');
check(textOf({ stage: 'silent', text: '' }) === '', 'a silent take has no text');
check(textOf({ stage: 'no_model' }) === '', 'a take without a model has no text');

console.log('');
console.log(`${checks - failures}/${checks} checks passed`);
process.exit(failures === 0 ? 0 : 1);
