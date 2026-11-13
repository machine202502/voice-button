// The phone's one HTTP entry point.
//
// Ported from `call()` in `src/web/js/app.js`. Every request to the computer
// goes through here so that three things cannot drift apart:
//
//   * the bearer token is added to every request (read per call, because the
//     pairing token can arrive after the page opened);
//   * a JSON body goes out as JSON and a binary body as octet-stream;
//   * one request has a 20 s deadline, because a computer that went away used
//     to hang the request and freeze the whole sync loop with it.
//
// The failures a caller must tell apart are three classes:
//
//   * `OfflineError` — `fetch` itself failed or the deadline fired: the
//     computer did not answer. Nothing was refused, so queued work stays;
//   * `PairingError` — HTTP 401: the token is missing or no longer accepted.
//     This is not a refusal either; the queue must wait for a new token;
//   * `ApiError` — the computer answered and said no. It carries the HTTP
//     status, which is what `sync.ts` reads (`statusOf`/`isRefusal`) to decide
//     whether to drop a queued change or leave it for the next tick.
//
// Neither `OfflineError` nor `PairingError` carries a `status`, so a refusal
// stays distinguishable from both without importing the sync loop.

/** What a caller may send: JSON-serialisable data, or bytes to stream. */
export type ApiBody = unknown;

/** What the computer answers: JSON, read field by field by the callers. */
export type ApiAnswer = Record<string, unknown>;

/** One request: method, path relative to `base`, optional JSON or binary body. */
export type Api = (method: string, path: string, body?: ApiBody) => Promise<ApiAnswer>;

/** The computer did not answer: `fetch` failed, or the deadline aborted it. */
export class OfflineError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'OfflineError';
  }
}

/**
 * The token was missing or is no longer accepted (HTTP 401).
 *
 * Deliberately carries no `status`: pairing is not a refusal of the request.
 */
export class PairingError extends Error {
  constructor(message = 'pairing required') {
    super(message);
    this.name = 'PairingError';
  }
}

/** The computer answered and said no; `status` is the HTTP status it used. */
export class ApiError extends Error {
  readonly status: number;

  constructor(message: string, status: number) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
  }
}

export interface CreateApiOptions {
  /** The prefix every path is appended to, e.g. `/site/api`. */
  base: string;
  /** The bearer token, read on every request; an empty string sends no header. */
  token: () => string;
  /** Injected by the tests; the page uses the global `fetch`. */
  fetch?: typeof fetch;
}

/** How long one request may take before it is aborted. */
const DEADLINE_MS = 20000;

/** The global `fetch`, kept bound to its global. */
const defaultFetch: typeof fetch = (input, init) => globalThis.fetch(input, init);

/** A JSON object, or `{}` when the answer is not one. */
function jsonObject(text: string): ApiAnswer {
  if (!text) return {};
  try {
    const parsed: unknown = JSON.parse(text);
    if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) return {};
    return parsed as Record<string, unknown>;
  } catch {
    return {};
  }
}

export function createApi({
  base,
  token,
  fetch: send = defaultFetch,
}: CreateApiOptions): Api {
  return async (method, path, body) => {
    const headers: Record<string, string> = {};
    const bearer = token();
    if (bearer) headers.Authorization = 'Bearer ' + bearer;
    let payload: BodyInit | undefined;
    if (body instanceof ArrayBuffer) {
      payload = body;
      headers['Content-Type'] = 'application/octet-stream';
    } else if (body instanceof Uint8Array) {
      // The DOM's `BodyInit` only accepts a view over an `ArrayBuffer`, while
      // `Uint8Array` is generic over `ArrayBufferLike`; a recorded chunk is the
      // former, so the narrowing is only for the type checker.
      payload = body as Uint8Array<ArrayBuffer>;
      headers['Content-Type'] = 'application/octet-stream';
    } else if (body !== undefined) {
      payload = JSON.stringify(body);
      headers['Content-Type'] = 'application/json';
    }
    const controller = new AbortController();
    const deadline = setTimeout(() => controller.abort(), DEADLINE_MS);
    let response: Response;
    try {
      response = await send(base + path, {
        method,
        headers,
        body: payload,
        signal: controller.signal,
        // The computer's answer is live state, not a document: a cached copy
        // would let an offline page undo a delete or an edit that the phone
        // already made. Offline it must fail, and the page then reads its own
        // cache (with the phone's changes applied) instead.
        cache: 'no-store',
      });
    } catch (error) {
      // No response at all: the computer is away, nothing was refused.
      throw new OfflineError(error instanceof Error ? error.message : String(error));
    } finally {
      clearTimeout(deadline);
    }
    if (response.status === 401) throw new PairingError();
    const answer = jsonObject(await response.text());
    if (!response.ok || answer.ok === false) {
      const reason = answer.error;
      const message =
        typeof reason === 'string' && reason ? reason : 'HTTP ' + String(response.status);
      throw new ApiError(message, response.status);
    }
    return answer;
  };
}
