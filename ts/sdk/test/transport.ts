// An in-memory transport: the test plays CAF's side.

import type { FromViewer, ToViewer, Transport } from "../src/index.ts";
import { Connection } from "../src/index.ts";

export class FakeTransport implements Transport {
  /** What the viewer sent, parsed. */
  readonly sent: FromViewer[] = [];
  closed = false;
  /** How many times `close` was called. */
  closes = 0;
  /** Make `send` throw, as a broken transport might. */
  broken = false;
  readonly #messages: ((text: string) => void)[] = [];
  readonly #closeListeners: (() => void)[] = [];

  send(text: string): void {
    if (this.broken) throw new Error("broken");
    this.sent.push(JSON.parse(text) as FromViewer);
  }

  onMessage(listener: (text: string) => void): void {
    this.#messages.push(listener);
  }

  onClose(listener: () => void): void {
    this.#closeListeners.push(listener);
  }

  close(): void {
    this.closes++;
    this.drop();
  }

  /** CAF sends a message. */
  deliver(message: ToViewer | { type: string; value?: unknown }): void {
    this.raw(JSON.stringify(message));
  }

  /** CAF sends any text at all. */
  raw(text: string): void {
    if (this.closed) return;
    for (const listener of this.#messages) listener(text);
  }

  /** The link drops. */
  drop(): void {
    if (this.closed) return;
    this.closed = true;
    for (const listener of this.#closeListeners) listener();
  }

  /** What was sent since the last look. */
  take(): FromViewer[] {
    return this.sent.splice(0);
  }
}

export const welcome: ToViewer = {
  type: "Welcome",
  value: { version: 1, agents: ["claude", "fake"], chats: [{ id: 1000, agent: "claude", cwd: "/home/me/project" }] },
};

/** A connection that has been welcomed, with the Hello already taken. */
export async function connected(): Promise<{ caf: Connection; link: FakeTransport }> {
  const link = new FakeTransport();
  const opening = Connection.open(link);
  link.deliver(welcome);
  const caf = await opening;
  link.take();
  return { caf, link };
}
