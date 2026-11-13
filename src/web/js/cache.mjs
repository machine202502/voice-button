// What the phone knows without the computer.
//
// The page is local-first: the monologs and the message pages it saw are kept
// in IndexedDB, and a monolog the user makes while the computer is away waits
// in the same place with a negative id. Nothing here talks to the network; the
// sync loop writes what the computer confirmed, the page reads pages and draws
// the visible window.
//
// Messages are never read as one array: `messagePage` is the only read, and it
// goes through the IndexedDB key range `['monolog', 'recorded', 'id']`. The
// decisions that matter (merge, negative ids, paging) are pure functions testable
// without a browser.

/** Next free local id: negative, one less than the lowest one already used. */
export function nextLocalId(keys = []) {
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
export function mergeMonologs(server = [], local = []) {
  const merged = [];
  const seen = new Set();
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

/**
 * What the page does when boot cannot reach the computer.
 *
 * Only a missing pairing token blocks the page. Once the phone is paired it can
 * always work: with a cache it shows what it knew, with an empty cache it still
 * opens an empty list, and a monolog created there waits for the computer. A
 * dead end after pairing was exactly what made the app look broken offline.
 */
export function bootOutcome({ hasToken = false } = {}) {
  return hasToken ? 'app' : 'pair';
}

/**
 * The cache itself, over the `monologs` key/value bag and the paged `messages`
 * store from storage.mjs. Records are plain data so IndexedDB can clone them.
 */
export function createCache({ monologs, messages, now = () => new Date().toISOString() }) {
  const keys = async () => (await monologs.keys()).map(Number).filter(Number.isFinite);

  /** Numeric server ids of one view, sorted by the server's own order. */
  async function serverRecords(archived = false) {
    const found = [];
    for (const key of await monologs.keys()) {
      const id = Number(key);
      if (!Number.isFinite(id) || id < 0) continue;
      const record = await monologs.get(key);
      if (!record || Boolean(record.archived) !== Boolean(archived)) continue;
      found.push({ ...record, id, local: false });
    }
    return found;
  }

  return {
    /**
     * One page of the monolog list, merged with the unsent local ones.
     *
     * The monolog list is small enough to keep the whole index in memory while
     * sorting (the messages are the big table). Only `limit` records are handed
     * to the page.
     */
    async monologPage(archived = false, offset = 0, limit = 100) {
      const size = Math.max(1, Math.min(500, Number(limit) || 100));
      const start = Math.max(0, Number(offset) || 0);
      const local = archived ? [] : await this.localMonologs();
      const merged = mergeMonologs(await serverRecords(archived), local);
      return {
        monologs: merged.slice(start, start + size),
        total: merged.length,
        hasMore: start + size < merged.length,
      };
    },

    /** How many server monologs of this view are on disk. */
    async serverMonologCount(archived = false) {
      return (await serverRecords(archived)).length;
    },

    /** Monologs made on the phone that still need the computer. */
    async localMonologs() {
      const found = [];
      for (const key of await monologs.keys()) {
        const id = Number(key);
        if (!Number.isFinite(id) || id >= 0) continue;
        const record = await monologs.get(key);
        if (record) found.push({ ...record, id, localId: id, local: true });
      }
      // Oldest first, like the takes: -2 was made after -1.
      found.sort((left, right) => left.id - right.id);
      return found;
    },

    /** Make a monolog locally; the sync loop creates it on the computer. */
    async allocate(name) {
      const id = nextLocalId(await monologs.keys());
      const record = {
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

    /** Remember which server monolog a local one became. */
    async mapLocal(localId, serverId) {
      const id = Number(localId);
      const remote = Number(serverId);
      await monologs.update(id, (previous) =>
        previous ? { ...previous, serverId: remote } : previous
      );
    },

    /** The server id of a local monolog, or null while it is unsent. */
    async serverId(localId) {
      const record = await monologs.get(Number(localId));
      if (!record || record.serverId == null) return null;
      const remote = Number(record.serverId);
      return Number.isFinite(remote) ? remote : null;
    },

    /** Read a record whatever key the bag returned for it. */
    async find(id) {
      let key = id;
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
    async rename(id, name) {
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
    async dirtyRenames() {
      const waiting = [];
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

    async clearRename(id) {
      const found = await this.find(id);
      if (!found) return;
      await monologs.put(found.key, { ...found.record, nameDirty: false });
    },

    /** Move one monolog between the open list and the archive, at once. */
    async setArchived(id, archived) {
      let key = id;
      let record = await monologs.get(key);
      if (!record) {
        key = String(id);
        record = await monologs.get(key);
      }
      if (!record) return false;
      await monologs.put(key, { ...record, archived: Boolean(archived) });
      return true;
    },

    /**
     * Store one server page. Pages are upserts: a page never deletes records it
     * did not contain. `complete` is for the one case the answer really is the
     * whole view (the first page when the computer says `has_more` is false):
     * then records of that view the computer no longer lists are dropped.
     * Local monologs (negative ids) are never touched.
     */
    async putServerMonologs(list = [], archived = false, { complete = false } = {}) {
      const incoming = new Map();
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
          if (Boolean(record.archived) === Boolean(archived)) await monologs.remove(key);
        }
      }
      for (const [id, monolog] of incoming) {
        const previous = await this.find(id);
        await monologs.put(id, {
          ...monolog,
          id,
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
     * for newer, neither for the newest page. This is the only message read.
     */
    async messagePage(monologId, { before = null, after = null, limit = 50 } = {}) {
      const monolog = Number(monologId);
      if (!Number.isFinite(monolog)) {
        return { messages: [], hasMore: false };
      }
      const page = await messages.page(monolog, { before, after, limit });
      return {
        messages: page.messages.map((message) => ({
          ...message,
          id: Number(message.id),
          monolog,
        })),
        hasMore: page.hasMore === true,
      };
    },

    /** Store one page of messages; merging is by the store's key, so no copy. */
    async putMessages(monologId, list = []) {
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

    /** How many messages one monolog has on this phone. */
    async messageCount(monologId) {
      return messages.count(Number(monologId));
    },

    /** Remove one message the computer no longer has. */
    async deleteMessage(monologId, message) {
      if (!message || typeof message !== 'object') return;
      await messages.remove(Number(monologId), message.recorded, message.id);
    },

    /** Is there anything readable on this phone at all? */
    async hasData() {
      if ((await keys()).length > 0) return true;
      return messages.any();
    },
  };
}
