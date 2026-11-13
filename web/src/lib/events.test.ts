import { describe, expect, it } from 'vitest';
import { createEventStream, type Revisions } from './events';

/** A response body the test feeds one frame at a time. */
class Channel {
  readonly response: Response;
  private readonly controller: ReadableStreamDefaultController<Uint8Array>;

  constructor(status = 200) {
    let opened: ReadableStreamDefaultController<Uint8Array> | null = null;
    const body = new ReadableStream<Uint8Array>({
      start(controller) {
        opened = controller;
      },
    });
    if (!opened) throw new Error('the stream did not open');
    this.controller = opened;
    this.response = new Response(body, { status, headers: { 'Content-Type': 'text/event-stream' } });
  }

  push(text: string): void {
    this.controller.enqueue(new TextEncoder().encode(text));
  }

  close(): void {
    this.controller.close();
  }
}

async function until(check: () => boolean): Promise<void> {
  for (let attempt = 0; attempt < 500; attempt += 1) {
    if (check()) return;
    await new Promise((resolve) => setTimeout(resolve, 1));
  }
  throw new Error('the condition never became true');
}

function revFrame(site: number, telegram: number): string {
  return `event: rev\ndata: {"site":${site},"telegram":${telegram}}\n\n`;
}

describe('the phone event stream', () => {
  it('sends the token in the header and calls back without waiting for a timer', async () => {
    const channel = new Channel();
    const seen: Revisions[] = [];
    const live: boolean[] = [];
    const retries: number[] = [];
    let headers: Record<string, string> = {};
    const stream = createEventStream({
      url: '/site/api/events',
      token: () => 'secret',
      onRev: (rev) => seen.push(rev),
      onLive: (value) => live.push(value),
      fetch: async (_input, init) => {
        headers = init?.headers as Record<string, string>;
        return channel.response;
      },
      schedule: (_callback, ms) => {
        retries.push(ms);
        return retries.length;
      },
      cancel: () => undefined,
    });

    stream.start();
    await until(() => live.includes(true));
    channel.push(revFrame(12, 7));
    await until(() => seen.length === 1);

    expect(headers.Authorization).toBe('Bearer secret');
    expect(headers.Accept).toBe('text/event-stream');
    expect(seen[0]).toEqual({ site: 12, telegram: 7 });
    expect(stream.live).toBe(true);
    expect(retries).toEqual([]);

    stream.close();
    channel.close();
    await new Promise((resolve) => setTimeout(resolve, 5));
    expect(retries, 'a stream this page closed must not reconnect').toEqual([]);
  });

  it('joins a frame split across chunks and ignores one that repeats the counters', async () => {
    const channel = new Channel();
    const seen: Revisions[] = [];
    const stream = createEventStream({
      url: '/site/api/events',
      token: () => 'secret',
      onRev: (rev) => seen.push(rev),
      fetch: async () => channel.response,
    });

    stream.start();
    channel.push('event: rev\ndata: {"site":1,');
    channel.push('"telegram":0}\n\n');
    await until(() => seen.length === 1);

    channel.push(revFrame(1, 0));
    channel.push(': ping\n\n');
    channel.push(revFrame(1, 2));
    await until(() => seen.length === 2);

    expect(seen).toEqual([
      { site: 1, telegram: 0 },
      { site: 1, telegram: 2 },
    ]);
    stream.close();
    channel.close();
  });

  it('does not reconnect when the token is refused', async () => {
    const channel = new Channel(401);
    let unauthorized = 0;
    const retries: number[] = [];
    const stream = createEventStream({
      url: '/site/api/events',
      token: () => 'old',
      onRev: () => undefined,
      onUnauthorized: () => {
        unauthorized += 1;
      },
      fetch: async () => channel.response,
      schedule: (_callback, ms) => {
        retries.push(ms);
        return retries.length;
      },
      cancel: () => undefined,
    });

    stream.start();
    await until(() => unauthorized === 1);
    expect(stream.live).toBe(false);
    expect(retries, 'a refused token is not retried').toEqual([]);
    channel.close();
  });

  it('backs off from one second to ten while the computer is away', async () => {
    const delays: number[] = [];
    const retries: Array<() => void> = [];
    const stream = createEventStream({
      url: '/site/api/events',
      token: () => 'secret',
      onRev: () => undefined,
      fetch: async () => {
        throw new Error('the computer is away');
      },
      schedule: (callback, ms) => {
        delays.push(ms);
        retries.push(callback);
        return delays.length;
      },
      cancel: () => undefined,
    });

    stream.start();
    await until(() => delays.length === 1);
    expect(delays).toEqual([1000]);
    for (let attempt = 0; attempt < 5; attempt += 1) {
      retries[attempt]!();
      await until(() => delays.length === attempt + 2);
    }
    expect(delays).toEqual([1000, 2000, 4000, 8000, 10000, 10000]);
    expect(stream.live).toBe(false);
    stream.close();
  });

  it('resets the backoff once an event arrives', async () => {
    const channel = new Channel();
    const delays: number[] = [];
    const retries: Array<() => void> = [];
    const seen: Revisions[] = [];
    let healthy = false;
    const stream = createEventStream({
      url: '/site/api/events',
      token: () => 'secret',
      onRev: (rev) => seen.push(rev),
      fetch: async () => {
        if (!healthy) throw new Error('the computer is away');
        return channel.response;
      },
      schedule: (callback, ms) => {
        delays.push(ms);
        retries.push(callback);
        return delays.length;
      },
      cancel: () => undefined,
    });

    stream.start();
    await until(() => delays.length === 1);
    retries[0]!();
    await until(() => delays.length === 2);
    expect(delays).toEqual([1000, 2000]);

    healthy = true;
    retries[1]!();
    await until(() => stream.live);
    channel.push(revFrame(4, 0));
    await until(() => seen.length === 1);

    // The stream works, so the next drop starts over at one second.
    channel.close();
    await until(() => delays.length === 3);
    expect(delays).toEqual([1000, 2000, 1000]);
    stream.close();
  });
});
