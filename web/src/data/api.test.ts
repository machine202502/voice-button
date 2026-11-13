// Tests for the phone's one HTTP entry point.
//
//   vitest run src/data
//
// There is no browser and no real network here: `fetch` is a fake that answers
// with a real `Response` (or throws, or waits for the deadline). The cases are
// the ones the sync loop depends on: the token, both body kinds, and the three
// failures it must tell apart — offline, pairing, refusal.
//
// The refusal/staleness distinction is checked with `sync.ts`'s own readers
// (`statusOf`, `isRefusal`), so the test fails if the two modules drift apart.

import { afterEach, describe, expect, it, vi } from 'vitest';

import { ApiError, OfflineError, PairingError, createApi } from './api';
import { isRefusal, statusOf } from './sync';
import type { Api as SyncApi } from './sync';

// ---------------------------------------------------------------- fakes

interface RecordedCall {
  url: string;
  init: RequestInit | undefined;
}

type Handler = (url: string, init: RequestInit | undefined) => Response | Promise<Response>;

/** A `fetch` that records each call and answers with the handler's answer. */
function fakeFetch(handler: Handler): { fetch: typeof fetch; calls: RecordedCall[] } {
  const calls: RecordedCall[] = [];
  const fake: typeof fetch = async (input, init) => {
    const url = typeof input === 'string' ? input : input instanceof URL ? input.href : input.url;
    calls.push({ url, init });
    return handler(url, init);
  };
  return { fetch: fake, calls };
}

/** One request header the fake received; our calls send a plain record. */
function headerOf(call: RecordedCall | undefined, name: string): string | undefined {
  const headers = call?.init?.headers;
  if (!headers) return undefined;
  const record = headers as Record<string, string>;
  return record[name];
}

/** The error a promise rejected with; fails when it resolved instead. */
async function caught(promise: Promise<unknown>): Promise<unknown> {
  try {
    await promise;
  } catch (error) {
    return error;
  }
  throw new Error('expected the call to fail');
}

function apiWith(
  handler: Handler,
  token: () => string = () => 'secret',
): {
  // The sync loop's own contract: this assignment is the structural check.
  api: SyncApi;
  calls: RecordedCall[];
} {
  const { fetch: fake, calls } = fakeFetch(handler);
  return { api: createApi({ base: '/site/api', token, fetch: fake }), calls };
}

// ---------------------------------------------------------------- tests

describe('the api entry point', () => {
  it('sends the bearer token on every request', async () => {
    const { api, calls } = apiWith(() => new Response('{}', { status: 200 }));
    await api('GET', '/monologues');
    expect(calls.length).toBe(1);
    expect(calls[0]?.url).toBe('/site/api/monologues');
    expect(headerOf(calls[0], 'Authorization')).toBe('Bearer secret');
    expect(calls[0]?.init?.method).toBe('GET');
  });

  it('sends a JSON body with the JSON content type', async () => {
    const { api, calls } = apiWith(() => new Response('{"ok":true}', { status: 200 }));
    const answer = await api('POST', '/messages', { monolog: 4, text: 'hi' });
    expect(answer).toEqual({ ok: true });
    expect(calls[0]?.init?.body).toBe('{"monolog":4,"text":"hi"}');
    expect(headerOf(calls[0], 'Content-Type')).toBe('application/json');
  });

  it('sends binary bodies untouched with the octet-stream content type', async () => {
    const { api, calls } = apiWith(() => new Response('{"ok":true}', { status: 200 }));
    const bytes = new Uint8Array([1, 2, 3]);
    await api('POST', '/messages/7/audio?seq=0', bytes);
    expect(calls[0]?.init?.body).toBe(bytes);
    expect(headerOf(calls[0], 'Content-Type')).toBe('application/octet-stream');

    const buffer = new Uint8Array([4, 5]).buffer;
    await api('POST', '/messages/7/audio?seq=1', buffer);
    expect(calls[1]?.init?.body).toBe(buffer);
    expect(headerOf(calls[1], 'Content-Type')).toBe('application/octet-stream');
  });

  it('turns a 401 into a pairing error that is not a refusal', async () => {
    const { api } = apiWith(() => new Response('', { status: 401 }));
    const error = await caught(api('GET', '/monologues'));
    expect(error).toBeInstanceOf(PairingError);
    expect(error).not.toBeInstanceOf(ApiError);
    expect(statusOf(error)).toBeNull();
    expect(isRefusal(error)).toBe(false);
  });

  it('turns a non-2xx into a refusal carrying the status', async () => {
    const { api } = apiWith(
      () =>
        new Response(JSON.stringify({ ok: false, error: 'this message is not receiving audio' }), {
          status: 409,
        }),
    );
    const error = await caught(api('POST', '/messages/7/audio?seq=0', new Uint8Array([1])));
    expect(error).toBeInstanceOf(ApiError);
    expect(statusOf(error)).toBe(409);
    expect(isRefusal(error)).toBe(true);
    expect((error as Error).message).toBe('this message is not receiving audio');
  });

  it('turns a network failure into an offline error, not a refusal', async () => {
    const { api } = apiWith(() => {
      throw new TypeError('Failed to fetch');
    });
    const error = await caught(api('GET', '/monologues'));
    expect(error).toBeInstanceOf(OfflineError);
    expect(error).not.toBeInstanceOf(ApiError);
    expect((error as Error).message).toBe('Failed to fetch');
    // `sync.ts` reads the status to tell a lost connection from a refusal.
    expect(statusOf(error)).toBeNull();
    expect(isRefusal(error)).toBe(false);
  });

  it('turns a 200 that says ok:false into a refusal too', async () => {
    const { api } = apiWith(
      () => new Response(JSON.stringify({ ok: false, error: 'not paired' }), { status: 200 }),
    );
    const error = await caught(api('GET', '/monologues'));
    expect(error).toBeInstanceOf(ApiError);
    expect((error as Error).message).toBe('not paired');
    // The port keeps the status the computer used, as the old `call()` did.
    expect(statusOf(error)).toBe(200);
  });
});

describe('the 20 s deadline', () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it('aborts a request the computer never answers and reports it as offline', async () => {
    vi.useFakeTimers();
    const { api, calls } = apiWith((_url, init) => {
      return new Promise<Response>((_resolve, reject) => {
        init?.signal?.addEventListener('abort', () => {
          reject(new DOMException('The operation was aborted.', 'AbortError'));
        });
      });
    });
    const failure = caught(api('GET', '/monologues'));
    await vi.advanceTimersByTimeAsync(20000);
    const error = await failure;
    expect(calls.length).toBe(1);
    expect(error).toBeInstanceOf(OfflineError);
    expect(statusOf(error)).toBeNull();
    expect(isRefusal(error)).toBe(false);
  });
});
