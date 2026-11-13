// What the phone shows for each stage, in one place.
//
// The page used to guess: a failed message with no speech looked like an error,
// a message without a model looked like it was still being transcribed, and the
// same take could appear twice. Now the stage is the only input, and this table
// is the only place that turns it into words and buttons.

/** Stages the computer can report, in the order a take goes through them. */
export const STAGES = [
  'draft',
  'uploading',
  'queued',
  'transcribing',
  'done',
  'silent',
  'no_model',
  'failed',
  'cancelled',
];

const TABLE = {
  draft: { label: 'draft', hint: 'draft_hint', tone: 'live', actions: ['delete'] },
  uploading: { label: 'uploading', hint: 'uploading_hint', tone: 'live', actions: ['delete'] },
  queued: { label: 'queued', hint: 'queued_hint', tone: 'live', actions: ['delete'] },
  transcribing: {
    label: 'transcribing',
    hint: 'transcribing_hint',
    tone: 'live',
    actions: ['delete'],
  },
  done: { label: 'done', hint: '', tone: 'done', actions: ['copy', 'edit', 'delete'] },
  silent: { label: 'silent', hint: 'silent_hint', tone: 'muted', actions: ['retry', 'delete'] },
  no_model: { label: 'no_model', hint: 'no_model_hint', tone: 'warning', actions: ['retry', 'delete'] },
  failed: { label: 'failed', hint: 'failed_hint', tone: 'warning', actions: ['retry', 'delete'] },
  cancelled: { label: 'cancelled', hint: '', tone: 'muted', actions: [] },
};

/**
 * What to draw for a message. An unknown stage is treated as `failed`: better a
 * "something went wrong, you can retry" than a card that pretends to work.
 */
export function describe(stage) {
  const known = TABLE[stage] ? stage : 'failed';
  const entry = TABLE[known];
  return {
    stage: known,
    label: entry.label,
    hint: entry.hint,
    tone: entry.tone,
    actions: [...entry.actions],
    /** Is the computer still working on it? Only then the page keeps polling. */
    busy: known === 'draft' || known === 'uploading' || known === 'queued' || known === 'transcribing',
  };
}

/** Does this stage still need the page to keep refreshing? */
export function keepsPolling(stages) {
  return stages.some((stage) => describe(stage).busy);
}

/**
 * The text of a card, once the stage has been considered. A message that is
 * still being written shows whatever the computer has so far; a silent one shows
 * nothing but its stage.
 */
export function textOf(message) {
  if (typeof message.text === 'string' && message.text.length > 0) return message.text;
  if (message.stage === 'silent') return '';
  return '';
}
