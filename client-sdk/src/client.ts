import type { ClientMessage, ServerMessage } from "./wire";

export type MessageHandler = (msg: ServerMessage) => void;
export type ErrorHandler = (err: Event) => void;

export interface Gp2fClientOptions {
  url: string;
  /** Called with every inbound {@link ServerMessage}. */
  onMessage: MessageHandler;
  /** Called on WebSocket error. */
  onError?: ErrorHandler;
  /** Called when the connection is established. */
  onOpen?: () => void;
  /** Called when the connection is closed. */
  onClose?: () => void;
  /**
   * Token-bucket capacity: maximum number of ops that may be sent in a burst.
   * Defaults to 10.
   */
  tokenBucketCapacity?: number;
  /**
   * Token-bucket refill rate in tokens per second.
   * Defaults to 5 (one token every 200 ms).
   */
  tokenBucketRefillRate?: number;
  /**
   * How long (ms) to pause optimistic updates after a conflict is detected.
   * Defaults to 500 ms ("Settle Duration").
   */
  conflictSettleMs?: number;
}

// ── Token Bucket ──────────────────────────────────────────────────────────────

/**
 * A simple Token Bucket rate limiter.
 *
 * Tokens refill continuously at `refillRate` tokens/second up to `capacity`.
 * Each `consume()` call removes one token.  If no token is available,
 * `consume()` returns the number of milliseconds to wait before retrying.
 */
class TokenBucket {
  private tokens: number;
  private lastRefill: number;

  constructor(
    private readonly capacity: number,
    private readonly refillRate: number, // tokens per second
  ) {
    this.tokens = capacity;
    this.lastRefill = Date.now();
  }

  /** Attempt to consume one token.  Returns 0 if successful or wait-ms > 0. */
  consume(): number {
    this.refill();
    if (this.tokens >= 1) {
      this.tokens -= 1;
      return 0;
    }
    // Calculate how long until the next token is available.
    // `1 - this.tokens` is the fractional deficit; convert to milliseconds.
    return Math.ceil(((1 - this.tokens) / this.refillRate) * 1_000);
  }

  private refill(): void {
    const now = Date.now();
    const elapsed = (now - this.lastRefill) / 1_000; // seconds
    this.tokens = Math.min(this.capacity, this.tokens + elapsed * this.refillRate);
    this.lastRefill = now;
  }
}

// ── Gp2fClient ────────────────────────────────────────────────────────────────

/**
 * GP2F WebSocket client with:
 * - Token-bucket rate limiting to prevent thundering-herd on reconnect.
 * - Settle-Duration: optimistic updates are paused for
 *   {@link Gp2fClientOptions.conflictSettleMs} after a conflict is detected.
 * - Retry-After: the client respects the server's backpressure hint.
 * - Time-offset tracking: the client records the delta between its clock and
 *   the server's clock reported in the `HELLO` message.
 */
export class Gp2fClient {
  private ws: WebSocket | null = null;
  private readonly options: Gp2fClientOptions;
  private readonly bucket: TokenBucket;

  /** Timestamp (Date.now()) until which sends are paused. */
  private pauseUntil = 0;

  /** Pending messages queued while the rate limiter or pause is active. */
  private readonly pendingQueue: ClientMessage[] = [];

  /** Drain timer handle (if set). */
  private drainTimer: ReturnType<typeof setTimeout> | null = null;

  /**
   * Difference `serverTimeMs - Date.now()` captured on the last HELLO.
   * Add this to `Date.now()` to get an estimate of the server's current time.
   */
  public serverTimeOffsetMs = 0;

  constructor(options: Gp2fClientOptions) {
    this.options = options;
    this.bucket = new TokenBucket(
      options.tokenBucketCapacity ?? 10,
      options.tokenBucketRefillRate ?? 5,
    );
  }

  /** Open the WebSocket connection. */
  connect(): void {
    if (this.ws) return;

    const ws = new WebSocket(this.options.url);
    this.ws = ws;

    ws.addEventListener("open", () => this.options.onOpen?.());
    ws.addEventListener("close", () => {
      this.ws = null;
      this.options.onClose?.();
    });
    ws.addEventListener("error", (e) => this.options.onError?.(e));
    ws.addEventListener("message", (e: MessageEvent<string>) => {
      try {
        const msg = JSON.parse(e.data) as ServerMessage;
        this.handleInbound(msg);
      } catch {
        // Ignore unparseable messages
      }
    });
  }

  /** Close the WebSocket connection. */
  disconnect(): void {
    this.ws?.close();
    this.ws = null;
  }

  /**
   * Send a {@link ClientMessage} to the server.
   *
   * If the rate limiter or a settle/retry-after pause is active the message
   * is queued and drained automatically once the pause expires.
   */
  send(msg: ClientMessage): void {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
      throw new Error("GP2F WebSocket is not connected");
    }

    const delay = this.nextSendDelay();
    if (delay > 0) {
      this.pendingQueue.push(msg);
      this.scheduleDrain(delay);
      return;
    }

    this.ws.send(JSON.stringify(msg));
  }

  /** Whether the connection is currently open. */
  get connected(): boolean {
    return this.ws?.readyState === WebSocket.OPEN;
  }

  // ── private ─────────────────────────────────────────────────────────────────

  /** Handle an inbound server message, updating internal rate-limit state. */
  private handleInbound(msg: ServerMessage): void {
    if (msg.type === "HELLO") {
      // Record the server-client time offset for HLC-aware scheduling.
      this.serverTimeOffsetMs = msg.serverTimeMs - Date.now();
    } else if (msg.type === "REJECT") {
      const settleMs = this.options.conflictSettleMs ?? 500;
      if (msg.retryAfterMs !== undefined) {
        // Server-side backpressure: respect the Retry-After hint.
        this.pauseUntil = Math.max(this.pauseUntil, Date.now() + msg.retryAfterMs);
      } else {
        // Conflict detected: apply the Settle Duration.
        this.pauseUntil = Math.max(this.pauseUntil, Date.now() + settleMs);
      }
      this.scheduleDrain(this.pauseUntil - Date.now());
    }
    this.options.onMessage(msg);
  }

  /**
   * Returns the number of milliseconds to wait before the next send.
   * 0 means "send immediately".
   */
  private nextSendDelay(): number {
    const pauseRemaining = Math.max(0, this.pauseUntil - Date.now());
    if (pauseRemaining > 0) return pauseRemaining;

    const bucketWait = this.bucket.consume();
    return bucketWait;
  }

  /** Schedule a drain of the pending queue after `delayMs` ms. */
  private scheduleDrain(delayMs: number): void {
    if (this.drainTimer !== null) return; // already scheduled
    this.drainTimer = setTimeout(() => {
      this.drainTimer = null;
      this.drainQueue();
    }, Math.max(0, delayMs));
  }

  /** Attempt to flush as many pending messages as the rate limiter allows. */
  private drainQueue(): void {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) return;

    while (this.pendingQueue.length > 0) {
      const delay = this.nextSendDelay();
      if (delay > 0) {
        this.scheduleDrain(delay);
        return;
      }
      const msg = this.pendingQueue.shift()!;
      this.ws.send(JSON.stringify(msg));
    }
  }
}
