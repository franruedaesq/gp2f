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
}

/**
 * Minimal GP2F WebSocket client.
 *
 * Manages a persistent WebSocket connection to the GP2F server and provides
 * a typed `send` method for {@link ClientMessage}s.
 */
export class Gp2fClient {
  private ws: WebSocket | null = null;
  private readonly options: Gp2fClientOptions;

  constructor(options: Gp2fClientOptions) {
    this.options = options;
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
        this.options.onMessage(msg);
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
   * Throws if the connection is not open.
   */
  send(msg: ClientMessage): void {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) {
      throw new Error("GP2F WebSocket is not connected");
    }
    this.ws.send(JSON.stringify(msg));
  }

  /** Whether the connection is currently open. */
  get connected(): boolean {
    return this.ws?.readyState === WebSocket.OPEN;
  }
}
