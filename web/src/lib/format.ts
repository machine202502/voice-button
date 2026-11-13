// Turning stored rows into the words and rows the phone shows.
//
// Everything here is pure: it reads a cached row and answers a string or a row
// list. The translation itself is the page's job (`page.tr`), because the
// language is reactive state and these helpers must not reach into it.

import type { Message } from '../data/types';

// ---------------------------------------------------------------- time

/** `2026-11-12T15:08:41+05:00` → `2026-11-12T15:08`, for a list card. */
export function cardDate(value: string | undefined): string {
  if (!value) return '';
  return String(value).replace('T', ' ').slice(0, 16);
}

/** The wall-clock time of a take: `15:08`. An unreadable stamp is empty. */
export function shortTime(iso: string | undefined): string {
  if (!iso) return '';
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return '';
  return `${String(date.getHours()).padStart(2, '0')}:${String(date.getMinutes()).padStart(2, '0')}`;
}

/** `2026-11-12T15:08:41+05:00` → `2026-11-12`. Anything unreadable is `''`. */
export function dayKey(recorded: string | undefined): string {
  const value = String(recorded ?? '');
  const cut = value.indexOf('T');
  return cut > 0 ? value.slice(0, cut) : '';
}

function localDayKey(date: Date): string {
  return (
    date.getFullYear() +
    '-' +
    String(date.getMonth() + 1).padStart(2, '0') +
    '-' +
    String(date.getDate()).padStart(2, '0')
  );
}

/**
 * A day separator: `today`/`yesterday` in the current language, otherwise the
 * locale's short weekday and date. `lang` is passed in so a language switch
 * redraws the separators too.
 */
export function dayLabel(key: string, lang: string): string {
  const today = new Date();
  if (key === localDayKey(today)) return 'today';
  const yesterday = new Date(today.getTime() - 86_400_000);
  if (key === localDayKey(yesterday)) return 'yesterday';
  const date = new Date(`${key}T00:00:00`);
  if (Number.isNaN(date.getTime())) return key;
  try {
    return date.toLocaleDateString(lang, { weekday: 'short', day: '2-digit', month: '2-digit' });
  } catch {
    return key;
  }
}

/**
 * The plural form the language needs for `count`: `messages_one`,
 * `messages_few` or `messages_many`. The caller translates it, so no language
 * has to be named here.
 */
export function pluralKey(count: number, lang: string): string {
  let form = 'many';
  try {
    const chosen = new Intl.PluralRules(lang).select(count);
    if (chosen === 'one' || chosen === 'few' || chosen === 'many') form = chosen;
  } catch {
    // An unknown locale means the simple many form.
  }
  return 'messages_' + form;
}

// ---------------------------------------------------------------- stages

/** Which actions a stage offers, in the order the card shows them. */
export type StageAction = 'copy' | 'edit' | 'retry' | 'delete';

export type StageTone = 'live' | 'muted' | 'warning' | 'done';

/** What one stage means: its catalog key, chip tone, actions and busy flag. */
export interface StageInfo {
  stage: string;
  /** Catalog key for the chip. */
  label: string;
  /** Catalog key for the text of an empty card, or `''`. */
  hint: string;
  tone: StageTone;
  actions: StageAction[];
  /** The computer is still working on it. */
  busy: boolean;
}

interface StageRow {
  label: string;
  hint: string;
  tone: StageTone;
  actions: StageAction[];
}

const STAGES: Record<string, StageRow> = {
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

const FALLBACK_STAGE: StageRow = {
  label: 'failed',
  hint: 'failed_hint',
  tone: 'warning',
  actions: ['retry', 'delete'],
};

/** The stage of a stored message; an unknown stage is treated as `failed`. */
export function describe(stage: string): StageInfo {
  const row = STAGES[stage] ?? FALLBACK_STAGE;
  const known = STAGES[stage] ? stage : 'failed';
  return {
    stage: known,
    label: row.label,
    hint: row.hint,
    tone: row.tone,
    actions: [...row.actions],
    busy: row.tone === 'live',
  };
}

// ---------------------------------------------------------------- messages

export function textOf(message: Message): string {
  return typeof message.text === 'string' ? message.text : '';
}

export function stageOf(message: Message): string {
  return typeof message.stage === 'string' ? message.stage : '';
}

export function isEdited(message: Message): boolean {
  return message.edited === true;
}

/** The computer is busy with the person in front of it; the take waits. */
export function isWaiting(message: Message): boolean {
  return message.waiting === true;
}

export function messageId(message: Message): number {
  return Number(message.id);
}

/** Oldest first, by the phone's own clock, with the id breaking ties. */
export function compareMessages(left: Message, right: Message): number {
  const a = String(left.recorded || '');
  const b = String(right.recorded || '');
  if (a < b) return -1;
  if (a > b) return 1;
  return Number(left.id) - Number(right.id);
}

/** Sorted and de-duplicated by id, so a page can never draw a row twice. */
export function dedupeMessages(list: readonly Message[]): Message[] {
  const seen = new Set<number>();
  const found: Message[] = [];
  for (const message of list) {
    const id = Number(message.id);
    if (!Number.isFinite(id) || seen.has(id)) continue;
    seen.add(id);
    found.push(message);
  }
  found.sort(compareMessages);
  return found;
}

/** One row of the feed: a day separator or one message. */
export type FeedRow =
  | { kind: 'day'; key: string; day: string }
  | { kind: 'message'; key: string; message: Message };

/**
 * The chat rows: oldest on top, newest at the bottom, one divider per day, and
 * nothing dropped — a message with an unreadable stamp gets a separator of its
 * own instead of disappearing.
 */
export function buildFeed(messages: readonly Message[]): FeedRow[] {
  const rows: FeedRow[] = [];
  let current: string | null = null;
  for (const message of dedupeMessages(messages)) {
    const day = dayKey(message.recorded);
    if (day !== current) {
      rows.push({ kind: 'day', key: 'day:' + day, day });
      current = day;
    }
    rows.push({ kind: 'message', key: 'msg:' + message.id, message });
  }
  return rows;
}
