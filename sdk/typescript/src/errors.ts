/**
 * Errors thrown by the client.
 *
 * `SyntraError` is the base: `HttpError` when the server answered with a
 * status outside 2xx, `TransportError` when no answer arrived. Messages
 * name the method, path and status, never the bearer token.
 */

/** Base class of the errors the client throws for a request. */
export class SyntraError extends Error {
  constructor(message: string, options?: ErrorOptions) {
    super(message, options);
    this.name = "SyntraError";
  }
}

export interface HttpErrorInit {
  method: string;
  /** Request path and query, without the origin. */
  path: string;
  status: number;
  /** The parsed JSON body, the raw text when it is not JSON, or undefined when empty. */
  body?: unknown;
  requestId?: string;
  retryAfterMs?: number;
  /** Defaults to `<method> <path>: HTTP <status>: <server message>`. */
  message?: string;
}

/** The server answered with a status outside 2xx: an error, or a redirect the client does not follow. */
export class HttpError extends SyntraError {
  readonly status: number;
  /**
   * The parsed JSON body (`{"error": "..."}` for Syntra's errors), the raw
   * text when it is not JSON, or undefined when it is empty.
   */
  readonly body: unknown;
  readonly method: string;
  /** Request path and query, without the origin. */
  readonly path: string;
  /** The response's `x-request-id`; the server's log lines carry it. */
  readonly requestId: string | undefined;
  /** `Retry-After` in milliseconds, when the answer had one (429, 503). */
  readonly retryAfterMs: number | undefined;

  constructor(init: HttpErrorInit) {
    super(init.message ?? httpErrorMessage(init.method, init.path, init.status, init.body));
    this.name = "HttpError";
    this.status = init.status;
    this.body = init.body;
    this.method = init.method;
    this.path = init.path;
    this.requestId = init.requestId;
    this.retryAfterMs = init.retryAfterMs;
  }
}

export interface TransportErrorInit {
  method: string;
  /** Request path and query, without the origin. */
  path: string;
  /** Set when the attempt was aborted after this many milliseconds. */
  timeoutMs?: number;
  /** The error `fetch` threw. */
  cause?: unknown;
  /** Defaults to `<method> <path>: <what failed>`. */
  message?: string;
}

/**
 * No answer: the connection was refused or reset, or the attempt timed
 * out. The request may or may not have reached the server.
 */
export class TransportError extends SyntraError {
  readonly method: string;
  /** Request path and query, without the origin. */
  readonly path: string;
  /** True when the attempt was aborted after the client's `timeoutMs`. */
  readonly timedOut: boolean;

  constructor(init: TransportErrorInit) {
    super(init.message ?? transportErrorMessage(init.method, init.path, init.timeoutMs, init.cause), {
      cause: init.cause,
    });
    this.name = "TransportError";
    this.method = init.method;
    this.path = init.path;
    this.timedOut = init.timeoutMs !== undefined;
  }
}

/** `POST /v1/...: HTTP 400: <the server's message>`. */
export function httpErrorMessage(method: string, path: string, status: number, body: unknown): string {
  const detail = serverMessage(body);
  return `${method} ${path}: HTTP ${status}${detail ? `: ${detail}` : ""}`;
}

export function transportErrorMessage(
  method: string,
  path: string,
  timeoutMs: number | undefined,
  cause: unknown,
): string {
  if (timeoutMs !== undefined) return `${method} ${path}: no response within ${timeoutMs} ms`;
  return `${method} ${path}: ${describe(cause)}`;
}

/** The message of a Syntra error body, a Personalizer-shaped one, or the start of a text body. */
function serverMessage(body: unknown): string | undefined {
  if (typeof body === "string") {
    const text = body.trim();
    return text.length > 200 ? `${text.slice(0, 200)}...` : text || undefined;
  }
  if (body !== null && typeof body === "object") {
    const error: unknown = (body as { error?: unknown }).error;
    if (typeof error === "string") return error;
    if (error !== null && typeof error === "object") {
      const message: unknown = (error as { message?: unknown }).message;
      if (typeof message === "string") return message;
    }
  }
  return undefined;
}

/** `fetch failed: connect ECONNREFUSED 127.0.0.1:8787` from undici's nested causes. */
function describe(cause: unknown): string {
  if (!(cause instanceof Error)) return `request failed: ${String(cause)}`;
  const inner = cause.cause;
  if (inner instanceof Error && inner.message && inner.message !== cause.message) {
    return `${cause.message}: ${inner.message}`;
  }
  return cause.message || cause.name;
}
