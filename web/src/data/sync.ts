// The only repeating work the phone page does.
//
// Everything that used to go wrong came from this one place: a timer that
// re-armed itself from three functions, a flush flag that could stick for ever,
// and a render that started another fetch. So the rules here are strict:
//
//   * `tick` never calls itself, never starts a timer and never renders more
//     than once per thing it draws;
//   * the guard is a plain boolean set at the start and cleared in `finally`,
//     so a failed network call cannot leave it stuck;
//   * monologs made on the phone are created on the computer first, and their
//     takes are only sent after the mapping is stored and written back into the
//     takes themselves;
//   * the outbox is sent in order and each item is only dropped after the
//     computer acknowledged it;
//   * the change queue is drained after the recordings, one entry at a time, in
//     the order the person acted: a lost connection stops the drain with the
//     queue untouched, and a refusal (4xx) drops the entry and is reported;
//   * the feed and the list are pages: `tick` fetches the newest feed page and
//     the first list page, the page asks for older/newer pages while scrolling.

import type { Cache, MessagePage } from './cache';
import type { MutationQueue } from './mutations';
import type { MessagePageOptions } from './storage';
import type {
  Message,
  MessageCursor,
  Monolog,
  Mutation,
  MutationKind,
  Take,
} from './types';

export type ApiBody = unknown;

/** What the computer answers: JSON, read field by field below. */
export type ApiAnswer = Record<string, unknown>;

export type Api = (method: string, path: string, body?: ApiBody) => Promise<ApiAnswer>;

export interface Chunk {
  seq: number;
  bytes: Uint8Array;
}

/** The recording outbox the page already has. */
export interface Outbox {
  list(): Promise<Take[]>;
  put(take: Take): Promise<void>;
  remove(localId: string): Promise<void>;
  chunks(localId: string): Promise<Chunk[]>;
  dropChunk(localId: string, seq: number): Promise<void>;
}

/** One list page: the server's answer, not the cache's window over it. */
export interface ListResult {
  monologs: Monolog[];
  hasMore: boolean;
  total: number | null;
}

/**
 * What the loop needs from the phone's cache: the local monologs the computer
 * has not seen, the server id of one that has, the paged feed, and the one
 * confirmed delete. It is the real `Cache`, narrowed, so the loop cannot drift
 * from it.
 */
export type SyncCache = Pick<
  Cache,
  | 'localMonologs'
  | 'mapLocal'
  | 'serverId'
  | 'putMessages'
  | 'messagePage'
  | 'messageCount'
  | 'putServerMonologs'
  | 'deleteMessage'
  | 'deleteMonolog'
  | 'commitEdit'
  | 'commitRename'
  | 'commitArchive'
>;

/** A change the computer refused for good, told to the person. */
export interface MutationFailure {
  seq: number;
  kind: MutationKind;
  reason: string;
}

/** What one tick did; the page reads only the fields it needs. */
export interface TickResult {
  skipped?: boolean;
  sent?: number;
  failed?: boolean;
  messages?: number;
  total?: number | null;
  monologs?: number;
  refused?: MutationFailure[];
}

export interface SyncOptions {
  api: Api;
  outbox: Outbox;
  /** The ordered queue of manual changes; drained after the recordings. */
  mutations: MutationQueue;
  cache: SyncCache;
  /** Draw one feed page; the waiting takes come with it. */
  render: (page: MessagePage, pending: Take[]) => void | Promise<void>;
  /** Draw the monolog list; absent while the page has no list screen. */
  renderList?: (page: ListResult, archived: boolean) => void | Promise<void>;
  /** The open monolog got a server id under the reader. */
  mapped?: (localId: number, serverId: number) => void;
  log?: (message: string) => void;
  pageSize?: number;
  listPageSize?: number;
}

export interface Sync {
  tick(): Promise<TickResult>;
  /** Which monolog the page shows; the tick fetches only this one. */
  show(id: number): void;
  /** Which list the page looks at: open monologs or the archive. */
  archive(value: boolean): void;
  /** One feed page for the page's scroll handler; cache first. */
  page(id: number, options?: MessagePageOptions): Promise<MessagePage>;
  /** One list page from the computer, for scrolling past the cached rows. */
  fetchMonologPage(
    archivedView: boolean,
    offset?: number,
    limit?: number,
  ): Promise<ListResult | null>;
  /** The server's total for a list view, or null while no answer had one. */
  monologTotal(archivedView: boolean): number | null;
  start(intervalMs?: number): void;
  stop(): void;
  flushOutbox(): Promise<TickResult>;
}

/** The API cursor for one message: the same `recorded|id` the server uses. */
export function messageCursor(message: MessageCursor | null | undefined): string | null {
  if (!message) return null;
  return String(message.recorded ?? '') + '|' + Number(message.id);
}

function pageQuery({ limit, before = null, after = null }: MessagePageOptions & { limit: number }): string {
  let query = '?limit=' + limit;
  if (before) query += '&before=' + encodeURIComponent(messageCursor(before) ?? '');
  if (after) query += '&after=' + encodeURIComponent(messageCursor(after) ?? '');
  return query;
}

/** A JSON object, or null when the answer is not one. */
function objectOf(value: unknown): Record<string, unknown> | null {
  if (value === null || typeof value !== 'object') return null;
  return value as Record<string, unknown>;
}

/**
 * The id the computer gave a created row: `{ monolog: { id } }` or `{ id }`,
 * and the same for a message.
 */
export function createdId(answer: ApiAnswer, key: 'monolog' | 'message'): number | null {
  const inner = objectOf(answer[key]);
  const id = Number(inner ? inner.id : answer.id);
  return Number.isFinite(id) && id > 0 ? id : null;
}

/** The messages of one answer, each one a row the cache can store. */
export function asMessages(value: unknown, monolog: number): Message[] {
  const list = Array.isArray(value) ? value : [];
  const found: Message[] = [];
  for (const item of list) {
    const record = objectOf(item);
    if (!record) continue;
    const id = Number(record.id);
    if (!Number.isFinite(id)) continue;
    const owner = Number(record.monolog);
    // The computer stamps an edit with its time, the page only asks whether
    // there is one: normalize the stamp to the boolean every screen reads.
    const edited = record.edited === true || (typeof record.edited === 'string' && record.edited !== '');
    found.push({
      ...record,
      edited,
      id,
      monolog: Number.isFinite(owner) ? owner : monolog,
      recorded: String(record.recorded ?? ''),
    });
  }
  return found;
}

/** The monologs of one answer, each one a row the list can draw. */
export function asMonologs(value: unknown): Monolog[] {
  const list = Array.isArray(value) ? value : [];
  const found: Monolog[] = [];
  for (const item of list) {
    const record = objectOf(item);
    if (!record) continue;
    const id = Number(record.id);
    if (!Number.isFinite(id) || id < 0) continue;
    found.push({
      ...record,
      id,
      name: typeof record.name === 'string' ? record.name : '',
    });
  }
  return found;
}

function totalOf(answer: ApiAnswer): number | null {
  const total = Number(answer.total);
  return Number.isFinite(total) ? total : null;
}

/** The HTTP status of a refusal, when the error carries one. */
export function statusOf(error: unknown): number | null {
  const record = objectOf(error);
  if (!record) return null;
  const status = Number(record.status);
  return Number.isFinite(status) ? status : null;
}

/** A 4xx is the computer saying no; anything else may pass later. */
export function isRefusal(error: unknown): boolean {
  const status = statusOf(error);
  return status !== null && status >= 400 && status < 500;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

/** What went to the computer for one mutation. */
interface MutationRequest {
  method: string;
  path: string;
  body?: unknown;
}

interface ResolvedTarget {
  monolog: number;
  message: number | null;
}

export function createSync({
  api,
  outbox,
  mutations,
  cache,
  render,
  renderList,
  mapped,
  log = () => {},
  pageSize = 50,
  listPageSize = 100,
}: SyncOptions): Sync {
  let running = false;
  let timer: ReturnType<typeof setInterval> | null = null;
  /** Which monolog the page shows; the tick fetches only this one. */
  let monologId = 0;
  /** Which list the page is looking at: open monologs or the archive. */
  let archived = false;
  /** The server's total for each list view, once an answer carried it. */
  const totals = new Map<string, number>();

  const viewKey = (value: boolean): string => (value ? '1' : '0');

  /** Create unsynced monologs and remember which server id each one got. */
  async function createLocalMonologs(): Promise<Map<number, number>> {
    const mapping = new Map<number, number>();
    for (const monolog of await cache.localMonologs()) {
      if (monolog.serverId != null) continue;
      const created = await api('POST', '/monologues', { name: monolog.name });
      const serverId = createdId(created, 'monolog');
      if (serverId === null) {
        throw new Error('the computer did not give the monolog an id');
      }
      await cache.mapLocal(monolog.localId, serverId);
      mapping.set(Number(monolog.localId), serverId);
      // The page may be inside this monolog right now: its id changes under
      // it, and asking for the old negative one would only get a 400.
      if (monologId === Number(monolog.localId)) {
        monologId = serverId;
        if (mapped) mapped(Number(monolog.localId), serverId);
      }
    }
    return mapping;
  }

  /** Rewrite takes of a newly mapped monolog before any of them is sent. */
  async function remapTakes(items: Take[], mapping: Map<number, number>): Promise<void> {
    for (const take of items) {
      const localId = Number(take.monolog);
      let serverId = mapping.get(localId);
      // A mapping stored by an earlier tick still has to reach the take, even
      // if this tick created a different monolog or failed after the first.
      if (serverId === undefined && localId < 0) {
        const stored = await cache.serverId(localId);
        if (stored !== null) serverId = stored;
      }
      if (serverId === undefined) continue;
      take.monolog = serverId;
      await outbox.put(take);
    }
  }

  /** Open one message for a take and return the id the computer gave it. */
  async function createMessage(item: Take): Promise<number> {
    const created = await api('POST', '/messages', {
      monolog: item.monolog,
      recorded: item.recorded,
    });
    const remote = createdId(created, 'message');
    if (remote === null) throw new Error('the computer did not give the take an id');
    return remote;
  }

  /** Send everything that waits, oldest first. Returns how many were sent. */
  async function flush(items: Take[]): Promise<number> {
    let sent = 0;
    for (const item of items) {
      // A take of a monolog the computer does not know yet must wait: sending a
      // negative id would be refused and the take would never catch up.
      if (Number(item.monolog) < 0) {
        log('take ' + item.localId + ' waits for its monolog');
        continue;
      }
      // A take that never reached the computer gets an id first.
      if (!item.remote) {
        item.remote = await createMessage(item);
        await outbox.put(item);
      }
      for (const chunk of await outbox.chunks(item.localId)) {
        let retried = false;
        for (;;) {
          try {
            await api(
              'POST',
              '/messages/' + item.remote + '/audio?seq=' + chunk.seq,
              chunk.bytes,
            );
            await outbox.dropChunk(item.localId, chunk.seq);
            break;
          } catch (error) {
            // The computer closes a take on its own after a long silence (the
            // phone went away). The rest of the recording is not lost: a fresh
            // message is opened for it and the same chunk is sent there. Before
            // this, one such refusal blocked the whole queue for ever.
            const status = statusOf(error);
            const refused = status === 409 || status === 404;
            if (refused && !retried) {
              retried = true;
              log('chunk ' + chunk.seq + ' refused; opening a new message for the rest: ' + status);
              item.remote = await createMessage(item);
              await outbox.put(item);
              continue;
            }
            if (refused) {
              log('chunk ' + chunk.seq + ' refused again; left for the next tick');
              break;
            }
            log('chunk ' + chunk.seq + ' failed: ' + String(error));
            throw error;
          }
        }
      }
      // Only a take the user closed may be closed on the computer, and only
      // once every chunk has left: removing the take also removes its audio,
      // which used to throw away the rest of the recording.
      if (item.closed) {
        const left = await outbox.chunks(item.localId);
        if (left.length > 0) continue;
        await api('POST', '/messages/' + item.remote + '/finish');
        await outbox.remove(item.localId);
      }
      sent += 1;
    }
    return sent;
  }

  /**
   * The server ids one mutation needs, or null while it cannot be sent.
   *
   * A mutation made before the computer knew its monolog is marked `local`:
   * the server id came from `createLocalMonologs` this tick, or from the cache
   * when an earlier tick stored it. Until it exists the mutation must wait in
   * place, because a later change must never overtake an earlier one.
   */
  async function resolveTarget(
    mutation: Mutation,
    mapping: Map<number, number>,
  ): Promise<ResolvedTarget | null> {
    const rawMonolog = Number(mutation.target.monolog);
    if (!Number.isFinite(rawMonolog)) return null;
    let monolog = rawMonolog;
    if (mutation.local || rawMonolog < 0) {
      const created = mapping.get(rawMonolog);
      const stored = created === undefined ? await cache.serverId(rawMonolog) : created;
      monolog = stored ?? 0;
    }
    if (monolog <= 0) return null;

    if (
      mutation.kind === 'monolog.rename' ||
      mutation.kind === 'monolog.archive' ||
      mutation.kind === 'monolog.delete'
    ) {
      return { monolog, message: null };
    }

    const rawMessage = Number(mutation.target.message);
    if (mutation.target.message === undefined || !Number.isFinite(rawMessage) || rawMessage <= 0) {
      // A message the computer has no id for cannot be edited or deleted there
      // yet: it waits for the take it belongs to.
      return null;
    }
    return { monolog, message: rawMessage };
  }

  /** The one request that replays a mutation. */
  function mutationRequest(mutation: Mutation, target: ResolvedTarget): MutationRequest | null {
    switch (mutation.kind) {
      case 'monolog.rename': {
        const { name } = mutation.payload;
        if (typeof name !== 'string') return null;
        return { method: 'PATCH', path: '/monologues/' + target.monolog, body: { name } };
      }
      case 'monolog.archive': {
        const { archived: value } = mutation.payload;
        // The page archives with POST and the router only answers POST here.
        if (typeof value !== 'boolean') return null;
        return {
          method: 'POST',
          path: '/monologues/' + target.monolog + '/archive',
          body: { archived: value },
        };
      }
      case 'monolog.delete':
        return { method: 'DELETE', path: '/monologues/' + target.monolog };
      case 'message.edit': {
        const { text } = mutation.payload;
        if (target.message === null || typeof text !== 'string') return null;
        return { method: 'PATCH', path: '/messages/' + target.message, body: { text } };
      }
      case 'message.delete':
        if (target.message === null) return null;
        return { method: 'DELETE', path: '/messages/' + target.message };
      default:
        return null;
    }
  }

  /**
   * Send the change queue in order. Returns the changes the computer refused.
   *
   * One entry at a time: a lost connection stops the drain with everything
   * still queued, so the next tick resumes at the same entry and no later
   * change overtakes an earlier one.
   */
  async function drainMutations(mapping: Map<number, number>): Promise<MutationFailure[]> {
    const refused: MutationFailure[] = [];
    for (;;) {
      const mutation = await mutations.take();
      if (!mutation) return refused;
      const target = await resolveTarget(mutation, mapping);
      if (target === null) {
        log('mutation ' + mutation.seq + ' waits for its monolog');
        return refused;
      }
      const request = mutationRequest(mutation, target);
      if (!request) {
        // Nothing can ever send this entry: drop it now instead of blocking the
        // queue for ever, and tell the person why.
        log('mutation ' + mutation.seq + ' carries no request');
        await mutations.remove(mutation.seq);
        refused.push({
          seq: mutation.seq,
          kind: mutation.kind,
          reason: 'incomplete mutation',
        });
        continue;
      }
      try {
        await api(request.method, request.path, request.body);
      } catch (error) {
        // A row the computer already deleted is the outcome we wanted: a 404 is
        // acceptance, not a refusal.
        const alreadyGone =
          (mutation.kind === 'message.delete' || mutation.kind === 'monolog.delete') &&
          statusOf(error) === 404;
        if (!alreadyGone) {
          if (!isRefusal(error)) {
            log('mutation ' + mutation.seq + ' failed: ' + String(error));
            return refused;
          }
          const reason = errorMessage(error);
          log('mutation ' + mutation.seq + ' refused: ' + reason);
          await mutations.remove(mutation.seq);
          refused.push({ seq: mutation.seq, kind: mutation.kind, reason });
          continue;
        }
        log('mutation ' + mutation.seq + ' already gone: ' + errorMessage(error));
      }
      // The computer has the change: write it into the cached row and drop the
      // local override there, before removing the entry. If the cache write
      // fails the mutation stays queued and the next tick retries it, instead
      // of leaving an override that would hide later changes for ever.
      await commitAccepted(mutation, target);
      await mutations.remove(mutation.seq);
    }
  }

  /** Store an accepted change in the cache so it stops being an override. */
  async function commitAccepted(mutation: Mutation, target: ResolvedTarget): Promise<void> {
    switch (mutation.kind) {
      case 'message.delete':
        // The confirmed delete removes the cached row and its tombstone
        // together, or the deleted message comes back on the next page.
        if (target.message !== null) {
          await cache.deleteMessage(target.monolog, target.message);
        }
        return;
      case 'monolog.delete':
        // Same rule for the monolog: row and tombstone go together, so a list
        // page that still has it cannot bring it back.
        await cache.deleteMonolog(target.monolog);
        return;
      case 'message.edit': {
        const { text } = mutation.payload;
        if (target.message !== null && typeof text === 'string') {
          await cache.commitEdit(target.monolog, target.message, text);
        }
        return;
      }
      case 'monolog.rename': {
        const { name } = mutation.payload;
        if (typeof name === 'string') await cache.commitRename(target.monolog, name);
        return;
      }
      case 'monolog.archive': {
        const { archived } = mutation.payload;
        if (typeof archived === 'boolean') await cache.commitArchive(target.monolog, archived);
        return;
      }
      default:
        return;
    }
  }

  /** The newest feed page from the computer, stored on the way. */
  async function fetchLatest(id: number): Promise<MessagePage> {
    const answer = await api('GET', '/monologues/' + id + '/messages?limit=' + pageSize);
    const page: MessagePage = {
      messages: asMessages(answer.messages, id),
      hasMore: answer.has_more === true,
      total: totalOf(answer),
    };
    await cache.putMessages(id, page.messages);
    return page;
  }

  /** The newest page this phone already has, when the computer is away. */
  async function cachedLatest(id: number): Promise<MessagePage> {
    const page = await cache.messagePage(id, { limit: pageSize });
    const total = await cache.messageCount(id);
    return {
      messages: page.messages,
      hasMore: page.messages.length < total || page.hasMore === true,
      total,
    };
  }

  /**
   * One page of a feed while the reader scrolls. IndexedDB first; the network
   * only when the stored page is short and might hide older/newer messages.
   */
  async function page(
    id: number,
    { before = null, after = null, limit = pageSize }: MessagePageOptions = {},
  ): Promise<MessagePage> {
    const cached = await cache.messagePage(id, { before, after, limit });
    const full = cached.hasMore === true || cached.messages.length >= limit;
    if (full) return cached;
    try {
      const answer = await api(
        'GET',
        '/monologues/' + id + '/messages' + pageQuery({ limit, before, after }),
      );
      const fetched: MessagePage = {
        messages: asMessages(answer.messages, id),
        hasMore: answer.has_more === true,
        total: totalOf(answer),
      };
      await cache.putMessages(id, fetched.messages);
      return fetched;
    } catch (error) {
      // The connection is down: what is stored is still readable. A refusal is
      // about the request and is handed to the caller.
      if (isRefusal(error)) throw error;
      return cached;
    }
  }

  /** Fetch one list page from the computer, store it and remember the total. */
  async function fetchMonologPage(
    archivedView: boolean,
    offset = 0,
    limit = listPageSize,
  ): Promise<ListResult | null> {
    const query =
      '?archived=' + (archivedView ? '1' : '0') +
      '&limit=' + limit +
      '&offset=' + Math.max(0, Number(offset) || 0);
    let answer: ApiAnswer;
    try {
      answer = await api('GET', '/monologues' + query);
    } catch (error) {
      log('list page failed: ' + String(error));
      return null;
    }
    const list = asMonologs(answer.monologs);
    const hasMore = answer.has_more === true;
    const total = totalOf(answer);
    if (total !== null) totals.set(viewKey(archivedView), total);
    await cache.putServerMonologs(list, archivedView, {
      // Only a first page the computer calls complete is the whole view.
      complete: offset === 0 && !hasMore,
    });
    return { monologs: list, total, hasMore };
  }

  /** Fetch the visible list view, store it and let the page draw it. */
  async function refreshList(): Promise<ListResult | null> {
    const page = await fetchMonologPage(archived, 0, listPageSize);
    if (!page) return null;
    if (renderList) await renderList(page, archived);
    return page;
  }

  /**
   * One unit of work: create local monologs, send what waits, drain the change
   * queue, fetch the open monolog or the list, draw.
   *
   * A second call while one is running is ignored — not queued, not counted —
   * so the page cannot spin.
   */
  async function tick(): Promise<TickResult> {
    if (running) return { skipped: true };
    running = true;
    try {
      const pending = await outbox.list();
      let sent = 0;
      let mappedNow = 0;
      let refused: MutationFailure[] = [];
      try {
        const mapping = await createLocalMonologs();
        mappedNow = mapping.size;
        // Run even with an empty mapping: a take may still carry a negative id
        // whose mapping is already stored from an earlier tick.
        await remapTakes(pending, mapping);
        sent = await flush(pending);
        refused = await drainMutations(mapping);
      } catch (error) {
        // The connection is down: the outbox and the change queue are
        // untouched and the next tick tries again. Nothing is lost.
        log('flush failed: ' + String(error));
        // The take the user just recorded still has to reach the screen: the
        // page draws the cached page (or what it last got) plus what waits, so
        // a recording made offline is visible as "not sent" instead of silence.
        if (monologId !== 0) {
          await render(await cachedLatest(monologId), await outbox.list());
        }
        return { sent: 0, failed: true, refused };
      }

      if (monologId !== 0) {
        // An open local monolog may have got its server id between ticks: ask
        // the mapped one, never the negative one.
        if (monologId < 0) {
          const remote = await cache.serverId(monologId);
          if (remote !== null) {
            const localId = monologId;
            monologId = remote;
            if (mapped) mapped(localId, remote);
          }
        }
        let page: MessagePage;
        try {
          page = await fetchLatest(monologId);
        } catch (error) {
          log('fetch failed: ' + String(error));
          await render(await cachedLatest(monologId), await outbox.list());
          return { sent, failed: true, refused };
        }
        await render(page, await outbox.list());
        // A monolog created this tick changes the list too: refresh it even
        // while its page is open, so going back does not show a stale cache.
        if (renderList && mappedNow > 0) await refreshList();
        return { sent, messages: page.messages.length, total: page.total, refused };
      }

      if (renderList) {
        const page = await refreshList();
        if (!page) return { sent, failed: true, refused };
        return { sent, monologs: page.monologs.length, total: page.total, refused };
      }
      return { sent, refused };
    } finally {
      running = false;
    }
  }

  return {
    tick,
    /** Which monolog the page shows; the tick fetches only this one. */
    show(id) {
      monologId = Number(id) || 0;
    },
    /** Which list the page is looking at; the tick fetches that view. */
    archive(value) {
      archived = Boolean(value);
    },
    /** One feed page for the page's scroll handler; cache first. */
    page,
    /** One list page from the computer, for scrolling past the cached rows. */
    fetchMonologPage,
    /** The server's total for a list view, or null while no answer had one. */
    monologTotal(archivedView) {
      const total = totals.get(viewKey(archivedView));
      return total !== undefined && Number.isFinite(total) ? total : null;
    },
    start(intervalMs = 1000) {
      if (timer) return;
      timer = setInterval(() => {
        tick().catch((error) => log('tick failed: ' + String(error)));
      }, intervalMs);
    },
    stop() {
      if (timer) clearInterval(timer);
      timer = null;
    },
    flushOutbox: tick,
  };
}
