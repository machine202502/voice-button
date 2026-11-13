// The shapes the phone's data layer passes around.
//
// Everything here is plain data: it is cloned into IndexedDB, so a value may
// only hold objects, arrays, strings, numbers, booleans and null. The ids are
// the ones the computer issues (numbers) or the phone makes up while offline:
// a monolog is negative, a message may be any valid IndexedDB key.
//
// This file is the one place the field names live. The queue and the sync loop
// construct the same records; a change here is a change to both.

/** An id the cache can key a row by: the computer's number or a local one. */
export type EntityId = number | string;

/** A monolog id: a server id, or a negative id while it is local. */
export type MonologId = number;

/** A message id: the computer's number, or a local id for an unsent take. */
export type MessageId = number | string;

/** A message as the API sends it: only `id` is guaranteed. */
export interface MessageInput {
  id: EntityId;
  monolog?: EntityId;
  recorded?: string;
  text?: string;
  [field: string]: unknown;
}

/** One message as the cache stores and returns it. */
export interface Message extends MessageInput {
  id: number;
  monolog: number;
  recorded: string;
}

/** Where a feed page starts or ends: the same `recorded|id` cursor the API uses. */
export interface MessageCursor {
  id: EntityId;
  recorded?: string;
}

/** A monolog as the API sends it: only `id` is guaranteed. */
export interface MonologInput {
  id: EntityId;
  name?: string;
  [field: string]: unknown;
}

/** One monolog as the list draws it and the cache stores it. */
export interface Monolog extends MonologInput {
  id: number;
  /** The server always names a monolog; a local one is named when it is made. */
  name: string;
  created?: string;
  count?: number;
  archived?: boolean;
  /** The newest message's words and stamp, for the list card. */
  last?: string;
  lastAt?: string;
  /** Something is recording or being transcribed in this monolog. */
  busy?: boolean;
  /** True while the monolog exists only on the phone. */
  local?: boolean;
  /** The negative id the monolog was allocated with. */
  localId?: number;
  /** The server id a local monolog became; null while it is unsent. */
  serverId?: number | null;
  /** A rename the old cache queued for the computer. */
  nameDirty?: boolean;
}

/** A monolog that exists only on the phone and waits for the computer. */
export interface LocalMonolog extends Monolog {
  localId: number;
  local: true;
  serverId: number | null;
}

/** One recording in the outbox, before the computer has all of it. */
export interface Take {
  localId: string;
  monolog: number;
  recorded: string;
  /** The message the computer opened for this take, once it did. */
  remote: number | null;
  /** The person stopped recording. */
  closed: boolean;
  /** The next audio chunk number. */
  seq: number;
}

/** One audio chunk of an unfinished take, numbered by the recorder. */
export interface Chunk {
  seq: number;
  bytes: Uint8Array;
}

/** Which table a local override belongs to. */
export type OverrideKind = 'monolog' | 'message';

/**
 * A manual change the phone made that the computer has not accepted yet.
 *
 * One record per row, keyed by `[kind, id]`, so a second change to the same row
 * merges into the first instead of competing with it. Only the fields the
 * person actually changed are present: a message carries `text` or `deleted`,
 * a monolog carries `name` or `archived`.
 */
export interface Override {
  kind: OverrideKind;
  id: EntityId;
  /** A tombstone: the row stays on disk and is hidden from every read. */
  deleted?: boolean;
  /** The locally edited text. */
  text?: string;
  /** The locally chosen archive state. */
  archived?: boolean;
  /** The locally chosen name. */
  name?: string;
  /** When the phone recorded the change. */
  at: string;
}

/** An action the computer can replay. */
export type MutationKind =
  | 'monolog.rename'
  | 'monolog.archive'
  | 'monolog.delete'
  | 'message.edit'
  | 'message.delete';

/** An id a mutation points at, before or after it has a server id. */
export type MutationId = EntityId;

/** What a mutation points at. A message always names its monolog. */
export interface MutationTarget {
  monolog: MutationId;
  message?: MutationId;
}

/** The arguments of the action: only the field its kind uses is set. */
export interface MutationPayload {
  name?: string;
  archived?: boolean;
  text?: string;
}

/** One entry in the ordered queue of manual changes. */
export interface Mutation {
  /** Assigned by the `mutations` store's key generator. */
  seq: number;
  kind: MutationKind;
  target: MutationTarget;
  payload: MutationPayload;
  /** The target has no server id yet: the loop substitutes one before sending. */
  local: boolean;
  createdAt: string;
}

/** The record as it is stored, before the store gives it a `seq`. */
export type MutationRecord = Omit<Mutation, 'seq'>;

/** What a caller hands to the queue; the store owns `seq`, the clock owns `createdAt`. */
export type MutationInput = Omit<Mutation, 'seq' | 'createdAt'> &
  Partial<Pick<Mutation, 'seq' | 'createdAt'>>;
