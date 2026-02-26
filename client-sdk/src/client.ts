import type { ClientMessage, ServerMessage } from "./wire";

export type MessageHandler = (msg: ServerMessage) => void;
export type ErrorHandler = (err: Event) => void;
/**
 * Called for each incremental text token received from the server during a
 * streaming AI response.  The `done` flag is `true` on the final token.
 */
export type TokenHandler = (token: string, done: boolean) => void;

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
   * Called with each incremental text token during a streaming AI response.
   * Enables token-by-token UI updates ("Time to First Token" UX pattern).
   */
  onToken?: TokenHandler;
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

// ── Optimistic UI ─────────────────────────────────────────────────────────────

/**
 * Options for {@link applyOptimisticUpdate}.
 */
export interface OptimisticUpdateOptions {
  /** The DOM element in which to render the loading indicator. */
  container: HTMLElement;
  /**
   * Vibe engine confidence in [0, 1].  When ≥ 0.7 a full skeleton loader is
   * shown; below that threshold a lighter "Thinking…" text badge is used.
   * Defaults to 0 (text badge).
   */
  confidence?: number;
  /**
   * Override the default "Thinking…" label shown in low-confidence mode.
   */
  thinkingText?: string;
}

/**
 * Show an optimistic UI loading indicator while waiting for an LLM response.
 *
 * Renders a skeleton loader (high-confidence path) or a "Thinking…" badge
 * (low-confidence path) inside `container`, then returns a cleanup function
 * that removes the indicator when the response arrives.
 *
 * @example
 * ```ts
 * const cleanup = applyOptimisticUpdate({ container: myDiv, confidence: 0.9 });
 * const response = await fetchAiSuggestion();
 * cleanup();
 * renderResponse(response);
 * ```
 */
export function applyOptimisticUpdate(options: OptimisticUpdateOptions): () => void {
  const { container, confidence = 0, thinkingText = "Thinking\u2026" } = options;

  const indicator = document.createElement("div");
  indicator.setAttribute("aria-live", "polite");
  indicator.setAttribute("aria-label", thinkingText);

  if (confidence >= 0.7) {
    // High-confidence: render a skeleton loader so the layout shift is minimal.
    indicator.setAttribute("data-gp2f-skeleton", "true");
    indicator.style.cssText = [
      "display:block",
      "background:linear-gradient(90deg,#e0e0e0 25%,#f5f5f5 50%,#e0e0e0 75%)",
      "background-size:200% 100%",
      "animation:gp2f-shimmer 1.4s infinite",
      "border-radius:4px",
      "height:1.2em",
      "width:80%",
      "margin:4px 0",
    ].join(";");
  } else {
    // Low-confidence: show a simple "Thinking…" text badge.
    indicator.setAttribute("data-gp2f-thinking", "true");
    indicator.textContent = thinkingText;
    indicator.style.cssText = "opacity:0.6;font-style:italic;font-size:0.9em";
  }

  // Inject the shimmer keyframes once per document.
  if (
    typeof document !== "undefined" &&
    !document.getElementById("gp2f-shimmer-style")
  ) {
    const style = document.createElement("style");
    style.id = "gp2f-shimmer-style";
    style.textContent =
      "@keyframes gp2f-shimmer{0%{background-position:200% 0}100%{background-position:-200% 0}}";
    document.head.appendChild(style);
  }

  container.appendChild(indicator);

  return () => {
    if (indicator.parentNode === container) {
      container.removeChild(indicator);
    }
  };
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
      // ── Streaming token path ───────────────────────────────────────────
      // The server may send incremental token frames before the final JSON
      // message to enable token-by-token UI updates (Time to First Token).
      // Streaming frames are plain-text lines of the form:
      //   data: <token>\n   (SSE-style, done=false)
      //   data: [DONE]\n    (final frame, done=true)
      if (this.options.onToken && e.data.startsWith("data: ")) {
        const payload = e.data.slice(6).trim();
        if (payload === "[DONE]") {
          this.options.onToken("", true);
        } else {
          this.options.onToken(payload, false);
        }
        return;
      }

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
