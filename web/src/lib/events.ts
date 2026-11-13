// The phone's push channel: one long-lived response from the computer carrying
// the two revision counters. While it is open nothing is polled; the fallback
// timer only earns its keep when the stream is down.
//
// The token travels in the `Authorization` header, not in the URL: `fetch` can
// send headers where `EventSource` cannot, so the pairing token does not land
// in a request log or in the browser's history.
//
// One response, one reader. The server writes an `event: rev` frame whenever a
// counter changes and a `: ping` comment every fifteen seconds; this file reads
// lines, keeps the last counters it saw, and calls `onRev` only when one grew.

export interface Revisions {
  site: number;
  telegram: number;
}

export interface EventStreamOptions {
  /** Where the stream lives, e.g. `/site/api/events`. */
  url: string;
  /** Read before every attempt: the token can arrive after the page opened. */
  token: () => string;
  /** A revision newer than the last one seen: refresh now, do not wait. */
  onRev: (rev: Revisions) => void;
  /** The stream is open (`true`) or dropped (`false`). */
  onLive?: (live: boolean) => void;
  /** The token is gone (HTTP 401): reconnect only after pairing again. */
  onUnauthorized?: () => void;
  /** Injected by the tests; the page uses the global `fetch`. */
  fetch?: typeof fetch;
  /** Injected by the tests; the page uses `setTimeout`. */
  schedule?: (callback: () => void, ms: number) => number;
  /** Injected by the tests; the page uses `clearTimeout`. */
  cancel?: (handle: number) => void;
}

export interface EventStream {
  /** Open the stream if it is not running; a no-op while it is. */
  start(): void;
  /** Close it and stay closed until `start` is called again. */
  close(): void;
  readonly live: boolean;
}

/** The first reconnect is quick; a computer that stays away is not hammered. */
const RETRY_MIN_MS = 1000;
/** A phone in a pocket must not retry faster than this forever. */
const RETRY_MAX_MS = 10_000;

const defaultFetch: typeof fetch = (input, init) => globalThis.fetch(input, init);

/** `site`/`telegram`, or `null` when an event is not a pair of numbers. */
function revisions(text: string): Revisions | null {
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    return null;
  }
  if (value === null || typeof value !== 'object') return null;
  const site = (value as Record<string, unknown>).site;
  const telegram = (value as Record<string, unknown>).telegram;
  if (typeof site !== 'number' || typeof telegram !== 'number') return null;
  if (!Number.isFinite(site) || !Number.isFinite(telegram)) return null;
  return { site, telegram };
}

export function createEventStream(options: EventStreamOptions): EventStream {
  const send = options.fetch ?? defaultFetch;
  const schedule =
    options.schedule ??
    ((callback: () => void, ms: number) => globalThis.setTimeout(callback, ms) as unknown as number);
  const cancel = options.cancel ?? ((handle: number) => globalThis.clearTimeout(handle));

  let active = false;
  let live = false;
  let retry = RETRY_MIN_MS;
  let timer: number | null = null;
  let controller: AbortController | null = null;
  /** The newest counters the computer has sent in this page's life. */
  let last: Revisions = { site: 0, telegram: 0 };

  function setLive(next: boolean): void {
    if (live === next) return;
    live = next;
    options.onLive?.(next);
  }

  function clearTimer(): void {
    if (timer !== null) {
      cancel(timer);
      timer = null;
    }
  }

  /** The connection ended by itself: retry after a growing pause. */
  function dropped(): void {
    setLive(false);
    if (!active) return;
    const delay = retry;
    retry = Math.min(retry * 2, RETRY_MAX_MS);
    clearTimer();
    timer = schedule(() => {
      timer = null;
      void attempt();
    }, delay);
  }

  /** One complete server-sent event. */
  function dispatch(name: string, data: string): void {
    if (name !== 'rev') return;
    const rev = revisions(data);
    if (!rev) return;
    // The stream works: the next drop starts its backoff from the beginning.
    retry = RETRY_MIN_MS;
    const newer = rev.site > last.site || rev.telegram > last.telegram;
    last = rev;
    if (newer) options.onRev(rev);
  }

  async function read(body: ReadableStream<Uint8Array>, owner: AbortController): Promise<void> {
    const reader = body.getReader();
    const decoder = new TextDecoder();
    let buffer = '';
    let event = '';
    let data: string[] = [];

    const line = (text: string): void => {
      if (text === '') {
        // A blank line ends the frame, whether or not it carried data.
        dispatch(event, data.join('\n'));
        event = '';
        data = [];
        return;
      }
      if (text.startsWith(':')) return; // a comment (`: ping`) keeps it alive
      const colon = text.indexOf(':');
      const field = colon < 0 ? text : text.slice(0, colon);
      let value = colon < 0 ? '' : text.slice(colon + 1);
      if (value.startsWith(' ')) value = value.slice(1);
      if (field === 'event') event = value;
      else if (field === 'data') data.push(value);
    };

    try {
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });
        let end = buffer.indexOf('\n');
        while (end >= 0) {
          let text = buffer.slice(0, end);
          if (text.endsWith('\r')) text = text.slice(0, -1);
          buffer = buffer.slice(end + 1);
          line(text);
          end = buffer.indexOf('\n');
        }
        if (!active || owner.signal.aborted) break;
      }
    } catch {
      // An aborted read is a page that closed the stream; a real error is a
      // dropped connection. Both end the loop and are reported the same way.
    } finally {
      try {
        await reader.cancel();
      } catch {
        // Already closed, or the browser is tearing the page down.
      }
    }
    if (active && !owner.signal.aborted) dropped();
  }

  async function attempt(): Promise<void> {
    if (!active) return;
    const owner = new AbortController();
    controller = owner;
    const headers: Record<string, string> = { Accept: 'text/event-stream' };
    const token = options.token();
    if (token) headers.Authorization = 'Bearer ' + token;

    let response: Response;
    try {
      response = await send(options.url, {
        method: 'GET',
        headers,
        signal: owner.signal,
        cache: 'no-store',
      });
    } catch {
      if (active && !owner.signal.aborted) dropped();
      return;
    }
    if (!active || owner.signal.aborted) return;
    if (response.status === 401) {
      // The token is no longer accepted. Reconnecting would only repeat the
      // refusal; the pairing screen is what has to happen next.
      active = false;
      clearTimer();
      setLive(false);
      options.onUnauthorized?.();
      return;
    }
    if (!response.ok || !response.body) {
      dropped();
      return;
    }
    setLive(true);
    await read(response.body, owner);
  }

  return {
    start(): void {
      if (active) return;
      active = true;
      retry = RETRY_MIN_MS;
      void attempt();
    },
    close(): void {
      active = false;
      clearTimer();
      controller?.abort();
      controller = null;
      setLive(false);
    },
    get live(): boolean {
      return live;
    },
  };
}
