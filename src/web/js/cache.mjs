// What the phone knows without the computer.
//
// The page is local-first: the last monologs and messages it saw are kept in
// IndexedDB, and a monolog the user makes while the computer is away waits in
// the same place with a negative id. Nothing here talks to the network; the
// sync loop writes what the computer confirmed, the page draws what is stored.
//
// The decisions are pure functions, so the cases that matter (merge, negative
// ids, the boot decision) are tested without a browser.

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
 * The cache itself, over two key/value bags from storage.mjs:
 * `monologs` keyed by id (negative for local) and `messages` keyed by monolog
 * id. Records are plain data so IndexedDB can clone them.
 */
export function createCache({ monologs, messages, now = () => new Date().toISOString() }) {
  const keys = async () => (await monologs.keys()).map(Number).filter(Number.isFinite);

  return {
    /** Monologs the computer confirmed, for the open list or the archive. */
    async serverMonologs(archived = false) {
      const found = [];
      for (const key of await monologs.keys()) {
        const id = Number(key);
        if (!Number.isFinite(id) || id < 0) continue;
        const record = await monologs.get(key);
        if (!record || Boolean(record.archived) !== Boolean(archived)) continue;
        found.push({ ...record, id, local: false });
      }
      found.sort((left, right) => right.id - left.id);
      return found;
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
     * Replace one view of the server list. Records of the other view stay, so
     * opening the archive does not throw away the open list, and an archived
     * monolog disappears from the open view when the computer stops listing it.
     */
    async putServerMonologs(list = [], archived = false) {
      const incoming = new Map();
      for (const monolog of list) {
        const id = Number(monolog.id);
        if (!Number.isFinite(id) || id < 0) continue;
        incoming.set(id, monolog);
      }
      for (const key of await monologs.keys()) {
        const id = Number(key);
        if (!Number.isFinite(id) || id < 0 || incoming.has(id)) continue;
        const record = await monologs.get(key);
        if (!record) continue;
        if (Boolean(record.archived) === Boolean(archived)) await monologs.remove(key);
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

    /** The last messages of one monolog the computer listed. */
    async messages(monologId) {
      const stored = await messages.get(Number(monologId));
      return Array.isArray(stored) ? stored : [];
    },

    async putMessages(monologId, list) {
      await messages.put(Number(monologId), Array.isArray(list) ? list : []);
    },

    /** Is there anything readable on this phone at all? */
    async hasData() {
      if ((await keys()).length > 0) return true;
      return (await messages.keys()).length > 0;
    },
  };
}
