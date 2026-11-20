import { afterEach, describe, expect, test, vi } from "vitest";

import { CafError, webSocket } from "../src/index.ts";

/** A stand-in for the browser's WebSocket, driven by the test. */
class FakeSocket extends EventTarget {
  static last: FakeSocket | null = null;
  readonly sent: string[] = [];
  closes = 0;

  constructor(readonly url: string) {
    super();
    FakeSocket.last = this;
  }

  send(text: string): void {
    this.sent.push(text);
  }

  close(): void {
    this.closes++;
    this.dispatchEvent(new Event("close"));
  }

  open(): void {
    this.dispatchEvent(new Event("open"));
  }

  message(data: unknown): void {
    this.dispatchEvent(new MessageEvent("message", { data }));
  }
}

function socket(): FakeSocket {
  const last = FakeSocket.last;
  if (last === null) throw new Error("no socket");
  return last;
}

afterEach(() => {
  vi.unstubAllGlobals();
  FakeSocket.last = null;
});

describe("webSocket", () => {
  test("resolves once open, and carries text both ways", async () => {
    vi.stubGlobal("WebSocket", FakeSocket);
    const opening = webSocket("ws://127.0.0.1:7320/viewer");
    expect(socket().url).toBe("ws://127.0.0.1:7320/viewer");
    socket().open();
    const transport = await opening;

    const heard: string[] = [];
    transport.onMessage((text) => heard.push(text));
    socket().message('{"type":"Ping"}');
    expect(heard).toStrictEqual(['{"type":"Ping"}']);

    transport.send('{"type":"Hello"}');
    expect(socket().sent).toStrictEqual(['{"type":"Hello"}']);
  });

  test("tells onClose once, however it closes", async () => {
    vi.stubGlobal("WebSocket", FakeSocket);
    const opening = webSocket("ws://x");
    socket().open();
    const transport = await opening;
    let closes = 0;
    transport.onClose(() => void closes++);
    transport.close();
    socket().dispatchEvent(new Event("close"));
    expect(closes).toBe(1);
    // A listener added after it has closed is still told.
    transport.onClose(() => void closes++);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(closes).toBe(2);
  });

  test("a binary message isn't CAF's: it closes the socket", async () => {
    vi.stubGlobal("WebSocket", FakeSocket);
    const opening = webSocket("ws://x");
    socket().open();
    const transport = await opening;
    const heard: unknown[] = [];
    transport.onMessage((text) => heard.push(text));
    socket().message(new ArrayBuffer(4));
    expect(heard).toStrictEqual([]);
    expect(socket().closes).toBe(1);
  });

  test("closing before it opens rejects with Disconnected", async () => {
    vi.stubGlobal("WebSocket", FakeSocket);
    const opening = webSocket("ws://x");
    socket().dispatchEvent(new Event("error"));
    socket().dispatchEvent(new Event("close"));
    const error = await opening.catch((error: unknown) => error);
    expect(error).toBeInstanceOf(CafError);
    expect((error as CafError).why).toStrictEqual({ type: "Disconnected" });
  });

  test("a URL the WebSocket won't take rejects with Disconnected", async () => {
    vi.stubGlobal(
      "WebSocket",
      class {
        constructor() {
          throw new SyntaxError("bad URL");
        }
      },
    );
    const error = await webSocket("nope").catch((error: unknown) => error);
    expect((error as CafError).why).toStrictEqual({ type: "Disconnected" });
  });
});
