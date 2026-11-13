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
//   * rendering is a function of the fetched list — the last one it received
//     when the computer is away, or the cache — plus whatever the outbox holds.

import { Offline } from './offline.mjs';

export function createSync({
  api,
  outbox,
  render,
  renderList = null,
  cache = null,
  mapped = null,
  log = () => {},
}) {
  let running = false;
  let timer = null;
  /** The last list the computer confirmed; drawn while it cannot be reached. */
  let known = [];
  /** Which list the page is looking at: open monologs or the archive. */
  let archived = false;

  /** Create unsynced monologs and remember which server id each one got. */
  async function createLocalMonologs() {
    const mapping = new Map();
    if (!cache) return mapping;
    for (const monolog of await cache.localMonologs()) {
      if (monolog.serverId != null) continue;
      const created = await api('POST', '/monologues', { name: monolog.name });
      const serverId = Number(created.monolog ? created.monolog.id : created.id);
      if (!Number.isFinite(serverId) || serverId <= 0) {
        throw new Error('the computer did not give the monolog an id');
      }
      await cache.mapLocal(monolog.localId, serverId);
      mapping.set(Number(monolog.localId), serverId);
      // The page may be inside this monolog right now: its id changes under
      // it, and asking for the old negative one would only get a 400.
      if (monologId === Number(monolog.localId)) {
        monologId = serverId;
        known = [];
        if (mapped) mapped(Number(monolog.localId), serverId);
      }
    }
    return mapping;
  }

  /** Rewrite takes of a newly mapped monolog before any of them is sent. */
  async function remapTakes(items, mapping) {
    for (const take of items) {
      let serverId = mapping.get(Number(take.monolog));
      // A mapping stored by an earlier tick still has to reach the take, even
      // if this tick created a different monolog or failed after the first.
      if (serverId === undefined && cache && Number(take.monolog) < 0) {
        serverId = await cache.serverId(take.monolog);
      }
      if (serverId === undefined || serverId === null) continue;
      take.monolog = serverId;
      await outbox.put(take);
    }
  }

  /** Send everything that waits, oldest first. Returns how many were sent. */
  async function flush(items) {
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
        const created = await api('POST', '/messages', {
          monolog: item.monolog,
          recorded: item.recorded,
        });
        item.remote = created.message ? created.message.id : created.id;
        if (!item.remote) throw new Error('the computer did not give the take an id');
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
            const refused =
              !(error instanceof Offline) &&
              (error.status === 409 || error.status === 404);
            if (refused && !retried) {
              retried = true;
              log('chunk ' + chunk.seq + ' refused; opening a new message for the rest: ' + error);
              const created = await api('POST', '/messages', {
                monolog: item.monolog,
                recorded: item.recorded,
              });
              item.remote = created.message ? created.message.id : created.id;
              if (!item.remote) throw new Error('the computer did not give the take an id');
              await outbox.put(item);
              continue;
            }
            if (refused) {
              log('chunk ' + chunk.seq + ' refused again; left for the next tick');
              break;
            }
            log('chunk ' + chunk.seq + ' failed: ' + error);
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

  /** What the page shows of one monolog while the computer cannot be asked. */
  async function fallback(id) {
    return cache ? cache.messages(id) : known;
  }

  /** Fetch one list view, store it and let the page draw it. */
  async function refreshList() {
    let answer;
    try {
      answer = await api('GET', '/monologues' + (archived ? '?archived=1' : ''));
    } catch (error) {
      // The cached list stays on screen; the loop will try again.
      log('list failed: ' + error);
      return null;
    }
    const list = answer.monologs || [];
    if (cache) await cache.putServerMonologs(list, archived);
    if (renderList) await renderList(list, archived);
    return list;
  }

  /**
   * One unit of work: create local monologs, send what waits, fetch the open
   * monolog or the list, draw.
   *
   * A second call while one is running is ignored — not queued, not counted —
   * so the page cannot spin.
   */
  async function tick() {
    if (running) return { skipped: true };
    running = true;
    try {
      const pending = await outbox.list();
      let sent = 0;
      let mappedNow = 0;
      try {
        const mapping = await createLocalMonologs();
        mappedNow = mapping.size;
        // Run even with an empty mapping: a take may still carry a negative id
        // whose mapping is already stored from an earlier tick.
        await remapTakes(pending, mapping);
        sent = await flush(pending);
        // A rename can wait: the list already shows the new name.
        if (cache) {
          for (const rename of await cache.dirtyRenames()) {
            await api('PATCH', '/monologues/' + rename.id, { name: rename.name });
            await cache.clearRename(rename.id);
          }
        }
      } catch (error) {
        // The connection is down: the outbox is untouched and the next tick
        // tries again. Nothing is lost, nothing is stuck.
        log('flush failed: ' + error);
        // The take the user just recorded still has to reach the screen: the
        // page draws the cache (or what it last got) plus what waits, so a
        // recording made offline is visible as "not sent" instead of silence.
        if (monologId !== 0) await render(await fallback(monologId), await outbox.list());
        return { sent: 0, failed: true, offline: error instanceof Offline };
      }

      if (monologId !== 0) {
        // An open local monolog may have got its server id between ticks: ask
        // the mapped one, never the negative one.
        if (cache && monologId < 0) {
          const remote = await cache.serverId(monologId);
          if (remote !== null) {
            const localId = monologId;
            monologId = remote;
            known = [];
            if (mapped) mapped(localId, remote);
          }
        }
        let answer;
        try {
          answer = await api('GET', '/monologues/' + monologId + '/messages');
        } catch (error) {
          log('fetch failed: ' + error);
          await render(await fallback(monologId), await outbox.list());
          return { sent, failed: true };
        }
        known = answer.messages || [];
        if (cache) await cache.putMessages(monologId, known);
        await render(known, await outbox.list());
        // A monolog created this tick changes the list too: refresh it even
        // while its page is open, so going back does not show a stale cache.
        if (renderList && mappedNow > 0) await refreshList();
        return { sent, messages: known.length };
      }

      if (renderList) {
        const list = await refreshList();
        if (!list) return { sent, failed: true };
        return { sent, monologs: list.length };
      }
      return { sent };
    } finally {
      running = false;
    }
  }

  let monologId = 0;

  return {
    tick,
    /** Which monolog the page shows; the tick fetches only this one. */
    show(id) {
      const next = Number(id) || 0;
      // The last list belongs to the monolog it was fetched for: a switch made
      // while the computer is unreachable must not paint another thread.
      if (next !== monologId) known = [];
      monologId = next;
    },
    /** Which list the page is looking at; the tick fetches that view. */
    archive(value) {
      archived = Boolean(value);
    },
    start(intervalMs = 1000) {
      if (timer) return;
      timer = setInterval(() => {
        tick().catch((error) => log('tick failed: ' + error));
      }, intervalMs);
    },
    stop() {
      if (timer) clearInterval(timer);
      timer = null;
    },
    flushOutbox: tick,
  };
}
