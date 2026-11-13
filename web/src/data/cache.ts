// What the phone knows without the computer, and what the person changed on it.
//
// The page is local-first: the monologs and the message pages it saw are kept
// in IndexedDB, and a monolog the user makes while the computer is away waits
// in the same place with a negative id.
//
// Manual changes do not rewrite those server rows. They are kept beside them as
// overrides in the `local` store, keyed by `[kind, id]`, and every read lays
// them over the row before returning it:
//
//   * a deleted message or monolog is a tombstone: hidden from every read while
//     the cached row stays on disk so the next server page cannot make it
//     reappear;
//   * an edited message returns the phone's text;
//   * a renamed monolog returns the phone's name;
//   * an archived monolog belongs to the archive list, not the open one.
//
// Nothing here talks to the network or the DOM. The bags are injected, so the
// same code runs on IndexedDB in the phone and on a map in the tests.

import type {
  EntityId,
  LocalMonolog,
  Message,
  MessageInput,
  Monolog,
  MonologInput,
  Override,
  OverrideKind,
} from './types';
import type { MessageStore, MessagePageOptions, StoreBag } from './storage';

/** Next free local id: negative, one less than the lowest one already used. */
export function nextLocalId(keys: readonly unknown[] = []): number {
  let lowest = 0;
  for (const key of keys) {
    const id = Number(key);
    if (Number.isFinite(id) && id < lowest) lowest = id;
  }
  return lowest - 1;
}

/**
 * Server monologs plus local ones that have not reached the computer.
 *
 * A local monolog that was already mapped is deduplicated by the mapping: the
 * server entry wins when the list has it, and the local name is used under the
 * server id until the next list arrives. A mapped monolog is never shown twice.
 */
export function mergeMonologs(
  server: readonly Monolog[] = [],
  local: readonly Monolog[] = [],
): Monolog[] {
  const merged: Monolog[] = [];
  const seen = new Set<number>();
  for (const monolog of server) {
    const id = Number(monolog.id);
    if (!Number.isFinite(id) || id < 0 || seen.has(id)) continue;
    seen.add(id);
    merged.push({ ...monolog, id, local: false });
  }
  for (const monolog of local) {
    const localId = Number(monolog.localId ?? monolog.id);
    if (!Number.isFinite(localId) || localId >= 0) continue;
    const mapped = monolog.serverId == null ? null : Number(monolog.serverId);
    if (mapped !== null && !Number.isFinite(mapped)) continue;
    if (mapped !== null && seen.has(mapped)) continue;
    if (mapped !== null) {
      seen.add(mapped);
      merged.push({ ...monolog, id: mapped, local: false, localId });
    } else {
      seen.add(localId);
      merged.push({ ...monolog, id: localId, local: true });
    }
  }
  // Newest first, in one place: the cache and the server list were sorted
  // differently, so rows swapped on every refresh.
  merged.sort((left, right) => {
    const a = String(left.created || '');
    const b = String(right.created || '');
    if (a !== b) return b.localeCompare(a);
    return Number(right.id) - Number(left.id);
  });
  return merged;
}

/** What the page does when boot cannot reach the computer. */
export interface BootState {
  hasToken?: boolean;
  offline?: boolean;
  hasCache?: boolean;
}

/**
 * Only a missing pairing token blocks the page. Once the phone is paired it can
 * always work: with a cache it shows what it knew, with an empty cache it still
 * opens an empty list, and a monolog created there waits for the computer.
 */
export function bootOutcome({ hasToken = false }: BootState = {}): 'app' | 'pair' {
  return hasToken ? 'app' : 'pair';
}

/** One page of the monolog list. */
export interface MonologPage {
  monologs: Monolog[];
  total: number;
  hasMore: boolean;
}

/** One page of the feed, oldest-first. */
export interface MessagePage {
  messages: Message[];
  hasMore: boolean;
  /** The feed is paged; a page never knows the total without counting. */
  total: number | null;
}

/** One manual change, merged into whatever the phone already had for that row. */
export type OverrideFields = Pick<Override, 'deleted' | 'text' | 'archived' | 'name'>;

/** Everything the page can ask the cache. */
export interface Cache {
  monologPage(archived?: boolean, offset?: number, limit?: number): Promise<MonologPage>;
  serverMonologCount(archived?: boolean): Promise<number>;
  localMonologs(): Promise<LocalMonolog[]>;
  allocate(name: string): Promise<LocalMonolog>;
  mapLocal(localId: number, serverId: number): Promise<void>;
  serverId(localId: number): Promise<number | null>;
  find(id: EntityId): Promise<{ key: IDBValidKey; record: Monolog } | null>;
  rename(id: EntityId, name: string): Promise<boolean>;
  dirtyRenames(): Promise<Array<{ id: number; name: string }>>;
  clearRename(id: EntityId): Promise<void>;
  setArchived(id: EntityId, archived: boolean): Promise<boolean>;
  putServerMonologs(
    list?: readonly MonologInput[],
    archived?: boolean,
    options?: { complete?: boolean },
  ): Promise<void>;
  messagePage(monologId: number, options?: MessagePageOptions): Promise<MessagePage>;
  putMessages(monologId: number, list?: readonly MessageInput[]): Promise<number>;
  messageCount(monologId: number): Promise<number>;
  message(monologId: number, messageId: MessageIdLike): Promise<Message | null>;
  /**
   * Remove one message the computer confirmed is gone: ids in, row gone,
   * tombstone gone. A local delete never calls this — it applies a tombstone.
   */
  deleteMessage(monologId: number, messageId: MessageIdLike): Promise<void>;
  /**
   * Remove one monolog the computer confirmed is gone, with its cached
   * messages and the tombstones that hid them. A local delete never calls
   * this — it applies a tombstone.
   */
  deleteMonolog(monologId: number): Promise<void>;
  /**
   * The computer accepted a local edit: write the accepted text into the
   * cached row, then drop the override. A page read between the two steps sees
   * the override's own (accepted) text, never the stale server row.
   */
  commitEdit(monologId: number, messageId: MessageIdLike, text: string): Promise<void>;
  /** The computer accepted a rename: write the name into the row, drop the override. */
  commitRename(monologId: number, name: string): Promise<void>;
  /** The computer accepted an archive state: write it into the row, drop the override. */
  commitArchive(monologId: number, archived: boolean): Promise<void>;
  hasData(): Promise<boolean>;
  applyEdit(message: MessageInput, text: string): Promise<Override>;
  applyDelete(message: MessageInput): Promise<Override>;
  applyArchive(monolog: MonologInput, archived: boolean): Promise<Override>;
  /** Hide a monolog on this phone. The computer has no delete route for it. */
  applyDeleteMonolog(monolog: MonologInput): Promise<Override>;
  applyRename(monolog: MonologInput, name: string): Promise<Override>;
  clearOverride(kind: OverrideKind, id: EntityId): Promise<void>;
  overrides(): Promise<Override[]>;
}

/** A message id accepted by `message`: the store normalizes it to a number. */
export type MessageIdLike = number | string;

/** The bags the cache needs: the same ones storage.mjs used, plus `local`. */
export interface CacheOptions {
  monologs: StoreBag<Monolog>;
  messages: MessageStore;
  /** The overrides bag, keyed by `['kind', 'id']`; `storage.local`. */
  local: StoreBag<Override>;
  now?: () => string;
}

/** One valid IndexedDB key for an override row. */
function overrideKey(kind: OverrideKind, id: EntityId): [OverrideKind, EntityId] {
  const number = Number(id);
  return [kind, Number.isFinite(number) ? number : String(id)];
}

/** A name or archive state from an override wins over the stored server value. */
function withMonologOverride(monolog: Monolog, override: Override | undefined): Monolog | null {
  if (!override) return monolog;
  if (override.deleted === true) return null;
  const next: Monolog = { ...monolog };
  if (typeof override.name === 'string') next.name = override.name;
  if (typeof override.archived === 'boolean') next.archived = override.archived;
  return next;
}

/** A tombstone hides the row; an edited text replaces the server's. */
function withMessageOverride(message: Message, override: Override | undefined): Message | null {
  if (!override) return message;
  if (override.deleted === true) return null;
  if (typeof override.text === 'string') return { ...message, text: override.text };
  return message;
}

/**
 * The cache itself, over the `monologs` bag, the paged `messages` store and the
 * `local` overrides bag from storage.ts. Records are plain data.
 */
export function createCache({
  monologs,
  messages,
  local,
  now = () => new Date().toISOString(),
}: CacheOptions): Cache {
  const keys = async (): Promise<number[]> =>
    (await monologs.keys()).map((key) => Number(key)).filter((id) => Number.isFinite(id));

  /** The override for one row, if the phone made one. */
  async function findOverride(kind: OverrideKind, id: EntityId): Promise<Override | undefined> {
    return local.get(overrideKey(kind, id));
  }

  /** Merge one change into the row's override, in one record. */
  async function writeOverride(
    kind: OverrideKind,
    id: EntityId,
    fields: OverrideFields,
  ): Promise<Override> {
    const key = overrideKey(kind, id);
    const previous = await local.get(key);
    const record: Override = {
      ...previous,
      ...fields,
      kind,
      id: key[1],
      at: now(),
    };
    await local.put(record);
    return record;
  }

  /** The row with the phone's own changes laid over it, or null if deleted. */
  async function withMessageRow(message: Message): Promise<Message | null> {
    return withMessageOverride(message, await findOverride('message', message.id));
  }

  /** Numeric server ids of one view, sorted by the server's own order. */
  async function serverRecords(archived = false): Promise<Monolog[]> {
    const found: Monolog[] = [];
    for (const key of await monologs.keys()) {
      const id = Number(key);
      if (!Number.isFinite(id) || id < 0) continue;
      const record = await monologs.get(key);
      if (!record) continue;
      const applied = withMonologOverride(
        { ...record, id, local: false },
        await findOverride('monolog', id),
      );
      if (!applied || Boolean(applied.archived) !== Boolean(archived)) continue;
      found.push(applied);
    }
    return found;
  }

  return {
    /**
     * One page of the monolog list, merged with the unsent local ones.
     *
     * Local monologs are placed in the view their override asks for, so an
     * archived local monolog shows in the archive and not in the open list.
     */
    async monologPage(archived = false, offset = 0, limit = 100): Promise<MonologPage> {
      const size = Math.max(1, Math.min(500, Number(limit) || 100));
      const start = Math.max(0, Number(offset) || 0);
      const local = (await this.localMonologs()).filter(
        (monolog) => Boolean(monolog.archived) === Boolean(archived),
      );
      const merged = mergeMonologs(await serverRecords(archived), local);
      return {
        monologs: merged.slice(start, start + size),
        total: merged.length,
        hasMore: start + size < merged.length,
      };
    },

    /** How many server monologs of this view are on disk. */
    async serverMonologCount(archived = false): Promise<number> {
      return (await serverRecords(archived)).length;
    },

    /** Monologs made on the phone that still need the computer. */
    async localMonologs(): Promise<LocalMonolog[]> {
      const found: LocalMonolog[] = [];
      for (const key of await monologs.keys()) {
        const localId = Number(key);
        if (!Number.isFinite(localId) || localId >= 0) continue;
        const record = await monologs.get(key);
        if (!record) continue;
        const server = record.serverId == null ? null : Number(record.serverId);
        const serverId = server !== null && Number.isFinite(server) ? server : null;
        // A monolog that got a server id keeps its override under that id: the
        // sync loop moves it when the mapping is stored, and the mapped row is
        // still drawn here until the server list contains it.
        const applied = withMonologOverride(
          { ...record, id: localId, localId, local: true, serverId },
          await findOverride('monolog', serverId ?? localId),
        );
        if (!applied) continue;
        found.push({ ...applied, id: localId, localId, local: true, serverId });
      }
      // Oldest first, like the takes: -2 was made after -1.
      found.sort((left, right) => left.id - right.id);
      return found;
    },

    /** Make a monolog locally; the sync loop creates it on the computer. */
    async allocate(name: string): Promise<LocalMonolog> {
      const id = nextLocalId(await monologs.keys());
      const record: LocalMonolog = {
        id,
        localId: id,
        name,
        created: now(),
        serverId: null,
        local: true,
      };
      await monologs.put(id, record);
      return record;
    },

    /**
     * Remember which server monolog a local one became.
     *
     * The local override made before the mapping (a rename, an archive) moves
     * with it, so the first server list cannot quietly undo it.
     */
    async mapLocal(localId: number, serverId: number): Promise<void> {
      const id = Number(localId);
      const remote = Number(serverId);
      if (!Number.isFinite(id) || !Number.isFinite(remote)) return;
      const source = overrideKey('monolog', id);
      const target = overrideKey('monolog', remote);
      if (source[1] !== target[1]) {
        const moved = await local.get(source);
        if (moved) {
          await local.put({ ...moved, id: target[1], at: now() });
          await local.remove(source);
        }
      }
      await monologs.update(id, (previous) =>
        previous ? { ...previous, serverId: remote } : previous,
      );
    },

    /** The server id of a local monolog, or null while it is unsent. */
    async serverId(localId: number): Promise<number | null> {
      const record = await monologs.get(Number(localId));
      if (!record || record.serverId == null) return null;
      const remote = Number(record.serverId);
      return Number.isFinite(remote) ? remote : null;
    },

    /** Read a record whatever key the bag returned for it. */
    async find(id: EntityId): Promise<{ key: IDBValidKey; record: Monolog } | null> {
      let key: IDBValidKey = id;
      let record = await monologs.get(key);
      if (!record) {
        key = String(id);
        record = await monologs.get(key);
      }
      return record ? { key, record } : null;
    },

    /**
     * Rename now, whatever the connection is doing. A monolog the computer has
     * not seen yet only changes here; one it knows gets `nameDirty`, and the
     * sync loop sends the PATCH when the computer answers.
     */
    async rename(id: EntityId, name: string): Promise<boolean> {
      const found = await this.find(id);
      if (!found) return false;
      const server = Number(id) >= 0;
      await monologs.put(found.key, {
        ...found.record,
        name,
        nameDirty: server,
      });
      return true;
    },

    /** Renames still waiting for the computer. */
    async dirtyRenames(): Promise<Array<{ id: number; name: string }>> {
      const waiting: Array<{ id: number; name: string }> = [];
      for (const key of await monologs.keys()) {
        const id = Number(key);
        if (!Number.isFinite(id) || id < 0) continue;
        const record = await monologs.get(key);
        if (record && record.nameDirty && record.name) {
          waiting.push({ id, name: record.name });
        }
      }
      return waiting;
    },

    async clearRename(id: EntityId): Promise<void> {
      const found = await this.find(id);
      if (!found) return;
      await monologs.put(found.key, { ...found.record, nameDirty: false });
    },

    /** Move one stored monolog between the open list and the archive. */
    async setArchived(id: EntityId, archived: boolean): Promise<boolean> {
      const found = await this.find(id);
      if (!found) return false;
      await monologs.put(found.key, { ...found.record, archived: Boolean(archived) });
      return true;
    },

    /**
     * Store one server page. Pages are upserts: a page never deletes records it
     * did not contain. `complete` is for the one case the answer really is the
     * whole view (the first page when the computer says `has_more` is false):
     * then records of that view the computer no longer lists are dropped.
     * Local monologs (negative ids) are never touched, and neither is a row the
     * person placed by hand: a list fetched before the change must not delete
     * the monolog the archive view is showing.
     */
    async putServerMonologs(
      list: readonly MonologInput[] = [],
      archived = false,
      { complete = false }: { complete?: boolean } = {},
    ): Promise<void> {
      const incoming = new Map<number, MonologInput>();
      for (const monolog of list) {
        const id = Number(monolog.id);
        if (!Number.isFinite(id) || id < 0) continue;
        incoming.set(id, monolog);
      }
      if (complete) {
        for (const key of await monologs.keys()) {
          const id = Number(key);
          if (!Number.isFinite(id) || id < 0 || incoming.has(id)) continue;
          const record = await monologs.get(key);
          if (!record) continue;
          const override = await findOverride('monolog', id);
          // The archive state is the phone's promise until the computer accepts
          // it: a complete page of the other view says nothing about this row.
          if (override && typeof override.archived === 'boolean') continue;
          if (Boolean(record.archived) === Boolean(archived)) await monologs.remove(key);
        }
      }
      for (const [id, monolog] of incoming) {
        const previous = await this.find(id);
        await monologs.put(id, {
          ...monolog,
          id,
          name: typeof monolog.name === 'string' ? monolog.name : '',
          archived: Boolean(archived),
          local: false,
          // A rename made while offline must survive the next list fetch.
          ...(previous && previous.record.nameDirty
            ? { name: previous.record.name, nameDirty: true }
            : {}),
        });
      }
    },

    /**
     * One page of the feed, oldest-first: `before` for older messages, `after`
     * for newer, neither for the newest page.
     *
     * Deleted messages are dropped after the page is read; `hasMore` is the
     * store's own answer, because a tombstone says nothing about the rows
     * beyond the page.
     */
    async messagePage(
      monologId: number,
      { before = null, after = null, limit = 50 }: MessagePageOptions = {},
    ): Promise<MessagePage> {
      const monolog = Number(monologId);
      if (!Number.isFinite(monolog)) {
        return { messages: [], hasMore: false, total: null };
      }
      const page = await messages.page(monolog, { before, after, limit });
      const applied: Array<Message | null> = [];
      for (const message of page.messages) {
        const record: Message = { ...message, id: Number(message.id), monolog };
        applied.push(await withMessageRow(record));
      }
      return {
        messages: applied.filter((message): message is Message => message !== null),
        hasMore: page.hasMore === true,
        total: null,
      };
    },

    /** Store one page of messages; merging is by the store's key, so no copy. */
    async putMessages(
      monologId: number,
      list: readonly MessageInput[] = [],
    ): Promise<number> {
      const monolog = Number(monologId);
      if (!Number.isFinite(monolog) || !Array.isArray(list) || list.length === 0) return 0;
      return messages.put(
        list.map((message) => ({
          ...message,
          id: Number(message.id),
          monolog,
          recorded: String(message.recorded ?? ''),
        })),
      );
    },

    /** How many messages one monolog has on this phone, tombstones excluded. */
    async messageCount(monologId: number): Promise<number> {
      const monolog = Number(monologId);
      if (!Number.isFinite(monolog)) return 0;
      const total = await messages.count(monolog);
      const hidden = new Set<number>();
      for (const override of await this.overrides()) {
        if (override.kind !== 'message' || override.deleted !== true) continue;
        const id = Number(override.id);
        if (Number.isFinite(id)) hidden.add(id);
      }
      if (hidden.size === 0) return total;
      let removed = 0;
      for (const id of await messages.ids(monolog)) {
        if (hidden.has(id)) removed += 1;
      }
      return Math.max(0, total - removed);
    },

    /** One message of one monolog, with the phone's changes applied. */
    async message(monologId: number, messageId: MessageIdLike): Promise<Message | null> {
      const monolog = Number(monologId);
      const id = Number(messageId);
      if (!Number.isFinite(monolog) || !Number.isFinite(id)) return null;
      const stored = await messages.find(monolog, id);
      if (!stored) return null;
      const record: Message = { ...stored, id: Number(stored.id), monolog };
      return withMessageRow(record);
    },

    /**
     * Remove one message the computer confirmed is gone, and drop the override
     * that went with it. The caller has ids only: the row is located here, so
     * the storage key (`recorded`) never leaks into the sync loop's contract.
     *
     * A local delete never calls this: it leaves a tombstone so a server page
     * cannot bring the row back before the computer agrees.
     */
    async deleteMessage(monologId: number, messageId: MessageIdLike): Promise<void> {
      const monolog = Number(monologId);
      const id = Number(messageId);
      if (!Number.isFinite(monolog) || !Number.isFinite(id)) return;
      const stored = await messages.find(monolog, id);
      if (stored) await messages.remove(monolog, String(stored.recorded), id);
      await this.clearOverride('message', id);
    },

    /**
     * Remove one monolog the computer confirmed is gone, with every message
     * cached under it and the overrides that hid them. The row goes before its
     * tombstone: a read between the two steps still sees the tombstone, never
     * a server row the delete already removed.
     */
    async deleteMonolog(monologId: number): Promise<void> {
      const monolog = Number(monologId);
      if (!Number.isFinite(monolog)) return;
      const ids = await messages.ids(monolog);
      // The server can reuse an id after deleting the last monolog, so its
      // cached feed must not become the new monolog's feed.
      await messages.removeMonolog(monolog);
      for (const id of ids) await this.clearOverride('message', id);
      const found = await this.find(monolog);
      if (found) await monologs.remove(found.key);
      await this.clearOverride('monolog', monolog);
    },

    /**
     * The computer accepted a local edit.
     *
     * The row is written first, then the override is dropped: if a page read
     * slips between the two writes it still lays the override (the accepted
     * text) over the row, so it can never show the old server text. A row that
     * is not cached is not an error — the override still goes.
     */
    async commitEdit(monologId: number, messageId: MessageIdLike, text: string): Promise<void> {
      const monolog = Number(monologId);
      const id = Number(messageId);
      if (Number.isFinite(monolog) && Number.isFinite(id)) {
        const stored = await messages.find(monolog, id);
        if (stored) {
          await messages.put([
            { ...stored, id, monolog, recorded: String(stored.recorded), text },
          ]);
        }
      }
      await this.clearOverride('message', messageId);
    },

    /** The computer accepted a rename: row first, then the override goes. */
    async commitRename(monologId: number, name: string): Promise<void> {
      const found = await this.find(monologId);
      if (found) await monologs.put(found.key, { ...found.record, name });
      await this.clearOverride('monolog', monologId);
    },

    /** The computer accepted an archive state: row first, then the override goes. */
    async commitArchive(monologId: number, archived: boolean): Promise<void> {
      const found = await this.find(monologId);
      if (found) {
        await monologs.put(found.key, { ...found.record, archived: Boolean(archived) });
      }
      await this.clearOverride('monolog', monologId);
    },

    /** Is there anything readable on this phone at all? */
    async hasData(): Promise<boolean> {
      if ((await keys()).length > 0) return true;
      return messages.any();
    },

    /** Record a locally edited message. The server row is left untouched. */
    async applyEdit(message: MessageInput, text: string): Promise<Override> {
      return writeOverride('message', message.id, { text });
    },

    /** Record a locally deleted message as a tombstone. */
    async applyDelete(message: MessageInput): Promise<Override> {
      return writeOverride('message', message.id, { deleted: true });
    },

    /** Record the list a monolog belongs to, before the computer answers. */
    async applyArchive(monolog: MonologInput, archived: boolean): Promise<Override> {
      return writeOverride('monolog', monolog.id, { archived: Boolean(archived) });
    },

    /** Hide one monolog on this phone until the computer confirms the delete. */
    async applyDeleteMonolog(monolog: MonologInput): Promise<Override> {
      return writeOverride('monolog', monolog.id, { deleted: true });
    },

    /** Record a local name for a monolog. */
    async applyRename(monolog: MonologInput, name: string): Promise<Override> {
      return writeOverride('monolog', monolog.id, { name });
    },

    /** Drop one override: a failed change goes back to the server's value. */
    async clearOverride(kind: OverrideKind, id: EntityId): Promise<void> {
      await local.remove(overrideKey(kind, id));
    },

    /** Every override the phone holds, for the sync loop to inspect. */
    async overrides(): Promise<Override[]> {
      const found: Override[] = [];
      for (const key of await local.keys()) {
        const record = await local.get(key);
        if (!record) continue;
        if (record.kind !== 'monolog' && record.kind !== 'message') continue;
        found.push(record);
      }
      return found;
    },
  };
}
