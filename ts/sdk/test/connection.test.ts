import { afterEach, describe, expect, test, vi } from "vitest";

import {
  CafError,
  ChatCopy,
  Connection,
  type Change,
  type Chat,
  type DeepReadonly,
  type Item,
  type Turn,
  type WatchEvent,
} from "../src/index.ts";
import { busy, chat, numbered, turn, words } from "./chats.ts";
import { connected, FakeTransport, welcome } from "./transport.ts";

/** Waits for promises and microtasks to settle. */
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

/** What a promise came to, without waiting forever. */
async function outcome<T>(promise: Promise<T>): Promise<{ ok: T } | { error: unknown } | "pending"> {
  let result: { ok: T } | { error: unknown } | "pending" = "pending";
  promise.then(
    (ok) => (result = { ok }),
    (error: unknown) => (result = { error }),
  );
  await settle();
  return result;
}

/** A promise whose rejection is looked at later, so it isn't reported as unhandled. */
function held<T>(promise: Promise<T>): Promise<T> {
  promise.catch(() => {});
  return promise;
}

async function rejection(promise: Promise<unknown>): Promise<CafError["why"]> {
  const result = await outcome(promise);
  if (result === "pending" || !("error" in result))
    throw new Error(`expected a rejection, got ${JSON.stringify(result)}`);
  expect(result.error).toBeInstanceOf(CafError);
  return (result.error as CafError).why;
}

const append = (text: string): Change => ({ type: "Appended", value: { item: 1004, text } });
const picture = (chat: Chat, latest: number) => ({ type: "Picture", value: { chat, latest } }) as const;
const changes = (...list: ReturnType<typeof numbered>[]) =>
  ({ type: "Changes", value: { chat: 1000, changes: list } }) as const;
const wordsOf = (chat: DeepReadonly<Chat>) => chat.turns[1]!.items[0]!.kind;

/** A listener that keeps what it hears. */
function heard() {
  const events: WatchEvent[] = [];
  return { events, listener: (event: WatchEvent) => void events.push(event) };
}

/** A copy of chat 1000 at `latest`, as a connection to `run` made it. */
async function copyFrom(run: string, latest: number): Promise<ChatCopy> {
  const { caf, link } = await connected(run);
  const watch = caf.watch(1000, () => {});
  link.deliver(picture(busy(), latest));
  caf.disconnect();
  return watch.copy!;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("the handshake", () => {
  test("says Hello first, and is welcomed", async () => {
    const link = new FakeTransport();
    const opening = Connection.open(link);
    expect(link.take()).toStrictEqual([{ type: "Hello", value: { min: 1, max: 1 } }]);
    link.deliver(welcome);
    const caf = await opening;
    expect(caf.agents).toStrictEqual(["claude", "fake"]);
    expect(caf.chats).toStrictEqual([{ id: 1000, agent: "claude", cwd: "/home/me/project" }]);
    expect(caf.run).toBe("run-1");
    expect(link.closed).toBe(false);
  });

  test("no common version rejects, and ends the link", async () => {
    const link = new FakeTransport();
    const opening = Connection.open(link);
    link.deliver({ type: "NoCommonVersion", value: { min: 2, max: 3 } });
    expect(await rejection(opening)).toStrictEqual({ type: "NoCommonVersion", value: { min: 2, max: 3 } });
    expect(link.closed).toBe(true);
  });

  test("the link ending first rejects with Disconnected", async () => {
    const link = new FakeTransport();
    const opening = Connection.open(link);
    link.drop();
    expect(await rejection(opening)).toStrictEqual({ type: "Disconnected" });
  });

  test("a transport that can't send rejects with Disconnected", async () => {
    const link = new FakeTransport();
    link.broken = true;
    expect(await rejection(Connection.open(link))).toStrictEqual({ type: "Disconnected" });
    expect(link.closed).toBe(true);
  });

  test("a message of a later version first is ignored", async () => {
    const link = new FakeTransport();
    const opening = Connection.open(link);
    link.deliver({ type: "Motd", value: "hi" });
    link.deliver(welcome);
    await expect(opening).resolves.toBeInstanceOf(Connection);
  });

  test.each([
    ["text that isn't JSON", "{nope"],
    ["JSON that isn't a message", "[1, 2]"],
    ["a message with no type", JSON.stringify({ value: 1 })],
    ["a Welcome without its run", JSON.stringify({ type: "Welcome", value: { version: 1, agents: [], chats: [] } })],
    ["a Welcome without its agents", JSON.stringify({ type: "Welcome", value: { version: 1, chats: [] } })],
    [
      "a Welcome of a version it doesn't speak",
      JSON.stringify({ ...welcome, value: { ...welcome.value, version: 2 } }),
    ],
    [
      "a Welcome with a broken chat",
      JSON.stringify({ type: "Welcome", value: { version: 1, agents: [], chats: [{}] } }),
    ],
    ["a NoCommonVersion without its range", JSON.stringify({ type: "NoCommonVersion", value: {} })],
    ["another message of version 1", JSON.stringify({ type: "Chats", value: [] })],
  ])("%s first ends the link", async (_, text) => {
    const link = new FakeTransport();
    const opening = Connection.open(link);
    link.raw(text);
    expect(await rejection(opening)).toStrictEqual({ type: "Disconnected" });
    expect(link.closed).toBe(true);
    // And nothing after it changes that.
    link.deliver(welcome);
  });
});

describe("the list of chats", () => {
  test("follows Chats, until a listener stops", async () => {
    const { caf, link } = await connected();
    const lists: unknown[] = [];
    const stop = caf.onChats((chats) => lists.push(chats));
    const next = [
      { id: 1000, agent: "claude", cwd: "/a" },
      { id: 1100, agent: "fake", cwd: "/b" },
    ];
    link.deliver({ type: "Chats", value: next });
    expect(caf.chats).toStrictEqual(next);
    expect(lists).toStrictEqual([next]);
    stop();
    link.deliver({ type: "Chats", value: [] });
    expect(caf.chats).toStrictEqual([]);
    expect(lists).toHaveLength(1);
  });

  test("the same listener twice is two subscriptions", async () => {
    const { caf, link } = await connected();
    let calls = 0;
    const listener = () => void calls++;
    const first = caf.onChats(listener);
    caf.onChats(listener);
    first();
    link.deliver({ type: "Chats", value: [] });
    expect(calls).toBe(1);
  });
});

describe("watching a chat", () => {
  test("asks for it, draws its picture, then its changes", async () => {
    const { caf, link } = await connected();
    const { events, listener } = heard();
    const watch = caf.watch(1000, listener);
    expect(link.take()).toStrictEqual([{ type: "Watch", value: { chat: 1000, since: null } }]);
    expect(watch.copy).toBeNull();

    link.deliver(picture(busy(), 10));
    expect(events).toStrictEqual([{ type: "Chat", chat: busy() }]);
    expect(watch.copy?.latest).toBe(10);

    link.deliver(changes(numbered(11, append(" a")), numbered(12, append(" b"))));
    expect(events).toHaveLength(2);
    const last = events[1]!;
    expect(last.type === "Chat" && wordsOf(last.chat)).toStrictEqual({ type: "Words", value: "Hello a b" });
    expect(watch.copy?.latest).toBe(12);
    expect(link.take()).toStrictEqual([]);
  });

  test("changes already had tell the listener nothing", async () => {
    const { caf, link } = await connected();
    const { events, listener } = heard();
    caf.watch(1000, listener);
    link.deliver(picture(busy(), 10));
    link.deliver(changes(numbered(9, append(" x")), numbered(10, append(" y"))));
    link.deliver(changes());
    expect(events).toHaveLength(1);
  });

  test("ignores other chats, and changes before the picture", async () => {
    const { caf, link } = await connected();
    const { events, listener } = heard();
    caf.watch(1000, listener);
    link.deliver(changes(numbered(11, append(" a"))));
    link.deliver(picture({ ...busy(), id: 2000 }, 10));
    link.deliver({ type: "Changes", value: { chat: 2000, changes: [numbered(11, append(" a"))] } });
    link.deliver({ type: "Ended", value: { chat: 2000 } });
    expect(events).toStrictEqual([]);
    expect(link.take()).toStrictEqual([{ type: "Watch", value: { chat: 1000, since: null } }]);
  });

  test.each<[string, Change]>([
    ["a gap", append(" c")],
    ["a change it doesn't know", { type: "TurnRenamed", value: {} } as unknown as Change],
    ["a change that doesn't fit", { type: "Appended", value: { item: 9, text: "x" } }],
  ])("%s asks for the chat whole again, once", async (_, bad) => {
    const { caf, link } = await connected();
    const { events, listener } = heard();
    const watch = caf.watch(1000, listener);
    link.deliver(picture(busy(), 10));
    link.take();

    const no = bad.type === "Appended" && bad.value.item === 1004 ? 13 : 12;
    link.deliver(changes(numbered(11, append(" a")), numbered(no, bad)));
    expect(link.take()).toStrictEqual([
      { type: "Unwatch", value: { chat: 1000 } },
      { type: "Watch", value: { chat: 1000, since: null } },
    ]);
    // It kept what it could take, and told the listener.
    expect(watch.copy?.latest).toBe(11);
    expect(events).toHaveLength(2);

    // Changes still on their way are dropped until the picture comes,
    // rather than asking again.
    link.deliver(changes(numbered(14, append(" d"))));
    expect(link.take()).toStrictEqual([]);
    expect(events).toHaveLength(2);

    const whole = busy();
    link.deliver(picture(whole, 20));
    expect(events[2]).toStrictEqual({ type: "Chat", chat: whole });
    link.deliver(changes(numbered(21, append("!"))));
    const last = events[3]!;
    expect(last.type === "Chat" && wordsOf(last.chat)).toStrictEqual({ type: "Words", value: "Hello!" });
  });

  test("a picture mid-stream replaces the copy, whatever its number", async () => {
    const { caf, link } = await connected();
    const { events, listener } = heard();
    const watch = caf.watch(1000, listener);
    link.deliver(picture(busy(), 10));
    link.deliver(changes(numbered(11, append(" a"))));
    const first = watch.copy;
    link.deliver(picture(busy(), 5));
    expect(watch.copy).not.toBe(first);
    expect(watch.copy?.latest).toBe(5);
    expect(events[2]).toStrictEqual({ type: "Chat", chat: busy() });
    link.deliver(changes(numbered(6, append(" z"))));
    expect(watch.copy?.latest).toBe(6);
  });

  test("Ended ends it", async () => {
    const { caf, link } = await connected();
    const { events, listener } = heard();
    const watch = caf.watch(1000, listener);
    link.deliver(picture(busy(), 10));
    link.deliver({ type: "Ended", value: { chat: 1000 } });
    expect(events.at(-1)).toStrictEqual({ type: "Ended" });
    link.deliver(changes(numbered(11, append(" a"))));
    link.deliver(picture(busy(), 30));
    expect(events).toHaveLength(2);
    link.take();
    // Nothing to unwatch: CAF has ended it.
    watch.stop();
    expect(link.take()).toStrictEqual([]);
  });

  test("a chat that doesn't exist is Ended at once", async () => {
    const { caf, link } = await connected();
    const { events, listener } = heard();
    const watch = caf.watch(4242, listener);
    link.deliver({ type: "Ended", value: { chat: 4242 } });
    expect(events).toStrictEqual([{ type: "Ended" }]);
    expect(watch.copy).toBeNull();
  });

  test("stop unwatches, and hears nothing more", async () => {
    const { caf, link } = await connected();
    const { events, listener } = heard();
    const watch = caf.watch(1000, listener);
    link.deliver(picture(busy(), 10));
    link.take();
    watch.stop();
    watch.stop();
    expect(link.take()).toStrictEqual([{ type: "Unwatch", value: { chat: 1000 } }]);
    link.deliver(changes(numbered(11, append(" a"))));
    link.deliver({ type: "Ended", value: { chat: 1000 } });
    expect(events).toHaveLength(1);
  });

  test("a listener that stops its watch mid-batch hears no more", async () => {
    const { caf, link } = await connected();
    const events: WatchEvent[] = [];
    const watch = caf.watch(1000, (event) => {
      events.push(event);
      if (events.length === 2) watch.stop();
    });
    link.deliver(picture(busy(), 10));
    link.deliver(changes(numbered(11, append(" a"))));
    link.deliver(changes(numbered(12, append(" b"))));
    expect(events).toHaveLength(2);
  });

  test("two watches of one chat share CAF's watch", async () => {
    const { caf, link } = await connected();
    const one = heard();
    const two = heard();
    const first = caf.watch(1000, one.listener);
    link.deliver(picture(busy(), 10));
    link.deliver(changes(numbered(11, append(" a"))));
    link.take();

    // CAF starts its watch again for the second, so both get a picture.
    const from = new ChatCopy(busy(), 3);
    const second = caf.watch(1000, two.listener, from);
    expect(link.take()).toStrictEqual([{ type: "Watch", value: { chat: 1000, since: null } }]);
    link.deliver(picture(busy(), 11));
    link.deliver(changes(numbered(12, append(" b"))));
    expect(one.events).toHaveLength(4);
    expect(two.events).toHaveLength(2);
    expect(first.copy?.latest).toBe(12);
    expect(second.copy?.latest).toBe(12);

    // Both need the chat whole, and one asking gets both the picture.
    link.deliver(changes(numbered(14, append(" d"))));
    expect(link.take()).toStrictEqual([
      { type: "Unwatch", value: { chat: 1000 } },
      { type: "Watch", value: { chat: 1000, since: null } },
    ]);
    link.deliver(picture(busy(), 20));
    expect(first.copy?.latest).toBe(20);
    expect(second.copy?.latest).toBe(20);

    first.stop();
    expect(link.take()).toStrictEqual([]);
    second.stop();
    expect(link.take()).toStrictEqual([{ type: "Unwatch", value: { chat: 1000 } }]);
  });

  test("resumes from a copy, leaving that copy as it was", async () => {
    const from = await copyFrom("run-1", 10);
    const { caf, link } = await connected();
    const { events, listener } = heard();
    const watch = caf.watch(1000, listener, from);
    expect(link.take()).toStrictEqual([{ type: "Watch", value: { chat: 1000, since: 10 } }]);
    expect(watch.copy?.chat).toBe(from.chat);
    expect(watch.copy).not.toBe(from);

    // Up to date: nothing to draw.
    link.deliver(changes());
    expect(events).toStrictEqual([]);
    link.deliver(changes(numbered(10, append(" x")), numbered(11, append(" a"))));
    const last = events[0]!;
    expect(last.type === "Chat" && wordsOf(last.chat)).toStrictEqual({ type: "Words", value: "Hello a" });
    expect(from.latest).toBe(10);
    expect(wordsOf(from.chat)).toStrictEqual({ type: "Words", value: "Hello" });
  });

  test("resuming may get a picture instead", async () => {
    const { caf, link } = await connected();
    const { events, listener } = heard();
    const watch = caf.watch(1000, listener, await copyFrom("run-1", 10));
    link.deliver(picture(busy(), 5000));
    expect(events).toHaveLength(1);
    expect(watch.copy?.latest).toBe(5000);
  });

  test("a copy of another chat isn't resumed from", async () => {
    const { caf, link } = await connected();
    const watch = caf.watch(1000, () => {}, new ChatCopy({ ...busy(), id: 2000 }, 10));
    expect(link.take()).toStrictEqual([{ type: "Watch", value: { chat: 1000, since: null } }]);
    expect(watch.copy).toBeNull();
  });

  test("a copy from another run of CAF isn't resumed from", async () => {
    // A restarted CAF numbers its chats and changes from the start again,
    // so chat 1000's change 10 there is another chat's.
    const from = await copyFrom("run-1", 10);
    const { caf, link } = await connected("run-2");
    const { events, listener } = heard();
    const watch = caf.watch(1000, listener, from);
    expect(link.take()).toStrictEqual([{ type: "Watch", value: { chat: 1000, since: null } }]);
    expect(watch.copy).toBeNull();
    // Changes before the picture don't graft onto the old copy.
    link.deliver(changes(numbered(11, append(" a"))));
    expect(events).toStrictEqual([]);
    link.deliver(picture(busy(), 3));
    expect(watch.copy?.latest).toBe(3);
  });

  test("a copy the app made itself isn't resumed from", async () => {
    const { caf, link } = await connected();
    caf.watch(1000, () => {}, new ChatCopy(busy(), 10));
    expect(link.take()).toStrictEqual([{ type: "Watch", value: { chat: 1000, since: null } }]);
  });

  test("a copy resumed from can be resumed from again, on a third connection", async () => {
    const from = await copyFrom("run-1", 10);
    const second = await connected();
    const watch = second.caf.watch(1000, () => {}, from);
    second.link.deliver(changes(numbered(11, append(" a"))));
    second.caf.disconnect();
    const third = await connected();
    third.caf.watch(1000, () => {}, watch.copy!);
    expect(third.link.take()).toStrictEqual([{ type: "Watch", value: { chat: 1000, since: 11 } }]);
  });

  test("a chat that has Ended is watched afresh after", async () => {
    const from = await copyFrom("run-1", 10);
    const { caf, link } = await connected();
    caf.watch(1000, () => {});
    link.deliver({ type: "Ended", value: { chat: 1000 } });
    link.take();
    // No watch of it is left, so this one may resume, and is the last.
    const again = caf.watch(1000, () => {}, from);
    expect(link.take()).toStrictEqual([{ type: "Watch", value: { chat: 1000, since: 10 } }]);
    again.stop();
    expect(link.take()).toStrictEqual([{ type: "Unwatch", value: { chat: 1000 } }]);
  });

  test("a transport that answers inside send never draws an old chat after a new one", async () => {
    // Nothing says a transport can't deliver CAF's answer before send
    // returns: this one sends the picture as soon as it's asked for.
    class Eager extends FakeTransport {
      override send(text: string): void {
        super.send(text);
        const message = JSON.parse(text) as { type: string; value: { since?: unknown } };
        if (message.type === "Watch" && message.value.since === null && this.armed) {
          this.deliver(picture(chat([]), 30));
        }
      }
      armed = false;
    }
    const link = new Eager();
    const { caf } = await connected("run-1", link);
    const drawn: number[] = [];
    const watch = caf.watch(1000, (event) => {
      if (event.type === "Chat") drawn.push(event.chat.turns.length);
    });
    link.deliver(picture(busy(), 10));
    link.armed = true;
    link.deliver(changes(numbered(11, { type: "TurnAdded", value: turn(1100) }), numbered(13, append(" c"))));
    expect(drawn).toStrictEqual([2, 3, 0]);
    expect(watch.copy?.chat.turns).toHaveLength(0);
  });

  test("a listener that stops the last watch partway through a batch doesn't watch again", async () => {
    const { caf, link } = await connected();
    let drawn = 0;
    const watch = caf.watch(1000, () => {
      if (++drawn === 2) watch.stop();
    });
    link.deliver(picture(busy(), 10));
    link.take();
    // 11 is drawn, and the listener stops; 13 then needs a picture.
    link.deliver(changes(numbered(11, append(" a")), numbered(13, append(" c"))));
    expect(link.take()).toStrictEqual([{ type: "Unwatch", value: { chat: 1000 } }]);
  });

  test("a picture that comes while the listener draws isn't asked for again", async () => {
    const { caf, link } = await connected();
    let drawing = 0;
    const watch = caf.watch(1000, () => {
      // CAF's next picture arrives while the first change of the batch is drawn.
      if (drawing++ === 1) link.deliver(picture(busy(), 30));
    });
    link.deliver(picture(busy(), 10));
    link.take();
    link.deliver(changes(numbered(11, append(" a")), numbered(13, append(" c"))));
    expect(link.take()).toStrictEqual([]);
    expect(watch.copy?.latest).toBe(30);
  });

  test("after Ended, a resumed watch that meets a gap asks for the chat whole", async () => {
    const from = await copyFrom("run-1", 10);
    const { caf, link } = await connected();
    // Asked for whole, then ended before the picture came.
    caf.watch(1000, () => {});
    link.deliver({ type: "Ended", value: { chat: 1000 } });
    caf.watch(1000, () => {}, from);
    link.take();
    link.deliver(changes(numbered(12, append(" b"))));
    expect(link.take()).toStrictEqual([
      { type: "Unwatch", value: { chat: 1000 } },
      { type: "Watch", value: { chat: 1000, since: null } },
    ]);
  });

  test("a listener that throws is reported, and the link goes on", async () => {
    const reported: unknown[] = [];
    vi.stubGlobal("reportError", (error: unknown) => reported.push(error));
    const { caf, link } = await connected();
    const other = heard();
    caf.watch(1000, () => {
      throw new Error("the app's bug");
    });
    caf.watch(1000, other.listener);
    caf.onChats(() => {
      throw new Error("another");
    });
    link.deliver(picture(busy(), 10));
    link.deliver({ type: "Chats", value: [] });
    expect(reported.map((error) => (error as Error).message)).toStrictEqual(["the app's bug", "another"]);
    expect(other.events).toHaveLength(1);
    expect(link.closed).toBe(false);
    const sending = caf.stop(1000);
    link.deliver({ type: "Done", value: { request: 1, outcome: { type: "Done" } } });
    await expect(sending).resolves.toBeUndefined();
  });

  test("without reportError, a listener's error is thrown on its own", async () => {
    vi.stubGlobal("reportError", undefined);
    const { caf, link } = await connected();
    const thrown: unknown[] = [];
    const queue = vi.spyOn(globalThis, "queueMicrotask").mockImplementation((task) => {
      try {
        task();
      } catch (error) {
        thrown.push(error);
      }
    });
    caf.watch(1000, () => {
      throw new Error("the app's bug");
    });
    link.deliver(picture(busy(), 10));
    queue.mockRestore();
    expect(thrown).toHaveLength(1);
    expect(link.closed).toBe(false);
  });
});

describe("requests", () => {
  const message = { parts: [{ type: "Text" as const, value: "hi" }] };

  test("open resolves with the chat", async () => {
    const { caf, link } = await connected();
    const opening = caf.open({ agent: "claude", cwd: "/home/me", session: { type: "New" } });
    expect(link.take()).toStrictEqual([
      { type: "Open", value: { request: 1, open: { agent: "claude", cwd: "/home/me", session: { type: "New" } } } },
    ]);
    link.deliver({ type: "Done", value: { request: 1, outcome: { type: "Opened", value: 1100 } } });
    await expect(opening).resolves.toBe(1100);
  });

  test("send resolves with the turn", async () => {
    const { caf, link } = await connected();
    const sending = caf.send(1000, message);
    expect(link.take()).toStrictEqual([{ type: "Send", value: { request: 1, chat: 1000, message } }]);
    link.deliver({ type: "Done", value: { request: 1, outcome: { type: "Sent", value: 1200 } } });
    await expect(sending).resolves.toBe(1200);
  });

  test("stop, answer and close resolve when done", async () => {
    const { caf, link } = await connected();
    const stopping = caf.stop(1000);
    const answering = caf.answer(1300, { type: "Permission", value: "allow" });
    const closing = caf.close(1000);
    expect(link.take()).toStrictEqual([
      { type: "Stop", value: { request: 1, chat: 1000 } },
      { type: "Answer", value: { request: 2, question: 1300, answer: { type: "Permission", value: "allow" } } },
      { type: "Close", value: { request: 3, chat: 1000 } },
    ]);
    // Out of order: each by its number.
    for (const request of [3, 1, 2]) link.deliver({ type: "Done", value: { request, outcome: { type: "Done" } } });
    await expect(Promise.all([stopping, answering, closing])).resolves.toStrictEqual([undefined, undefined, undefined]);
  });

  test("Refused, NotAllowed and Gone reject with CafError", async () => {
    const { caf, link } = await connected();
    const refused = held(caf.send(1000, message));
    const later = held(caf.stop(1000));
    const notAllowed = held(caf.open({ agent: "claude", cwd: "/", session: { type: "New" } }));
    const gone = held(caf.close(1000));
    link.deliver({ type: "Done", value: { request: 1, outcome: { type: "Refused", value: { type: "QueueFull" } } } });
    // A reason of a later CAF's comes through, for the app to show by name.
    link.deliver({ type: "Done", value: { request: 2, outcome: { type: "Refused", value: { type: "Busy" } } } });
    link.deliver({ type: "Done", value: { request: 3, outcome: { type: "NotAllowed" } } });
    link.deliver({ type: "Done", value: { request: 4, outcome: { type: "Gone" } } });
    expect(await rejection(refused)).toStrictEqual({ type: "Refused", value: { type: "QueueFull" } });
    expect(await rejection(later)).toStrictEqual({ type: "Refused", value: { type: "Busy" } });
    expect(await rejection(notAllowed)).toStrictEqual({ type: "NotAllowed" });
    expect(await rejection(gone)).toStrictEqual({ type: "Gone" });
    expect(link.closed).toBe(false);
  });

  test("a Done for nothing asked is ignored", async () => {
    const { caf, link } = await connected();
    const stopping = caf.stop(1000);
    link.deliver({ type: "Done", value: { request: 99, outcome: { type: "Done" } } });
    expect(await outcome(stopping)).toBe("pending");
    link.deliver({ type: "Done", value: { request: 1, outcome: { type: "Done" } } });
    expect(await outcome(stopping)).toStrictEqual({ ok: undefined });
    // A second Done for the same request is for nothing now.
    link.deliver({ type: "Done", value: { request: 1, outcome: { type: "Sent", value: 1 } } });
    expect(link.closed).toBe(false);
  });

  test("an outcome of a later version rejects as Unknown, and the link goes on", async () => {
    const { caf, link } = await connected();
    const stopping = held(caf.stop(1000));
    const sending = caf.send(1000, message);
    link.deliver({ type: "Done", value: { request: 1, outcome: { type: "Postponed", value: { until: 5 } } } });
    expect(await rejection(stopping)).toStrictEqual({ type: "Unknown", value: { outcome: "Postponed" } });
    expect(link.closed).toBe(false);
    link.deliver({ type: "Done", value: { request: 2, outcome: { type: "Sent", value: 1200 } } });
    await expect(sending).resolves.toBe(1200);
  });

  test.each([
    ["an outcome that doesn't fit what was asked", { type: "Sent", value: 1200 }],
    ["an outcome with no type", { value: 1100 }],
    ["an outcome whose type isn't text", { type: 7 }],
    ["Opened without its chat", { type: "Opened" }],
    ["a Refused without its reason", { type: "Refused", value: "QueueFull" }],
  ])("%s ends the link", async (_, bad) => {
    const { caf, link } = await connected();
    const opening = held(caf.open({ agent: "claude", cwd: "/", session: { type: "New" } }));
    const other = held(caf.stop(1000));
    let closes = 0;
    caf.onClose(() => void closes++);
    link.deliver({ type: "Done", value: { request: 1, outcome: bad } });
    expect(await rejection(opening)).toStrictEqual({ type: "Disconnected" });
    expect(await rejection(other)).toStrictEqual({ type: "Disconnected" });
    expect(link.closed).toBe(true);
    expect(closes).toBe(1);
  });
});

describe("the link ending", () => {
  test("rejects every pending request, and tells onClose once", async () => {
    const { caf, link } = await connected();
    let closes = 0;
    caf.onClose(() => void closes++);
    const pending: Promise<unknown>[] = [
      held(caf.send(1000, { parts: [] })),
      held(caf.stop(1000)),
      held(caf.close(1000)),
    ];
    link.drop();
    for (const each of pending) expect(await rejection(each)).toStrictEqual({ type: "Disconnected" });
    expect(closes).toBe(1);
    link.drop();
    caf.disconnect();
    expect(closes).toBe(1);
  });

  test("disconnect ends it from the viewer's side", async () => {
    const { caf, link } = await connected();
    let closes = 0;
    caf.onClose(() => void closes++);
    const stopping = caf.stop(1000);
    link.take();
    caf.disconnect();
    expect(link.closed).toBe(true);
    expect(closes).toBe(1);
    expect(await rejection(stopping)).toStrictEqual({ type: "Disconnected" });
    // Nothing goes out after it, and nothing comes in.
    expect(await rejection(caf.send(1000, { parts: [] }))).toStrictEqual({ type: "Disconnected" });
    expect(() => caf.watch(1000, () => {})).toThrow(CafError);
    try {
      caf.watch(1000, () => {});
    } catch (error) {
      expect((error as CafError).why).toStrictEqual({ type: "Disconnected" });
    }
    expect(link.take()).toStrictEqual([]);
  });

  test("onClose gives a function that stops it", async () => {
    const { caf, link } = await connected();
    let closes = 0;
    const stop = caf.onClose(() => void closes++);
    caf.onClose(() => void closes++);
    stop();
    link.drop();
    expect(closes).toBe(1);
    // Stopped before it's called soon, it isn't.
    const late = caf.onClose(() => void closes++);
    late();
    await settle();
    expect(closes).toBe(1);
  });

  test("a watch isn't ended by it: onClose says so", async () => {
    const { caf, link } = await connected();
    const { events, listener } = heard();
    const watch = caf.watch(1000, listener);
    link.deliver(picture(busy(), 10));
    link.drop();
    expect(events).toHaveLength(1);
    expect(watch.copy?.latest).toBe(10);
  });

  test("onClose after it has ended is called soon", async () => {
    const { caf, link } = await connected();
    link.drop();
    let closes = 0;
    caf.onClose(() => void closes++);
    expect(closes).toBe(0);
    await settle();
    expect(closes).toBe(1);
  });

  test("a transport that can't send ends it", async () => {
    const { caf, link } = await connected();
    link.broken = true;
    expect(await rejection(caf.stop(1000))).toStrictEqual({ type: "Disconnected" });
    expect(link.closed).toBe(true);
  });
});

describe("what CAF sends", () => {
  test("a message of a type it doesn't know is ignored", async () => {
    const { caf, link } = await connected();
    const { events, listener } = heard();
    caf.watch(1000, listener);
    link.deliver({ type: "Typing", value: { chat: 1000 } });
    link.deliver({ type: "Ping" });
    link.deliver(picture(busy(), 10));
    expect(events).toHaveLength(1);
    expect(link.closed).toBe(false);
  });

  test.each([
    ["text that isn't JSON", "not json"],
    ["a number", "42"],
    ["null", "null"],
    ["a message whose type isn't text", JSON.stringify({ type: 1 })],
    ["Chats that isn't a list of chats", JSON.stringify({ type: "Chats", value: [{ id: "1000" }] })],
    ["a Picture without its number", JSON.stringify({ type: "Picture", value: { chat: busy() } })],
    [
      "a Picture of something that isn't a chat",
      JSON.stringify(picture({ ...busy(), turns: {} } as unknown as Chat, 1)),
    ],
    [
      "a Picture whose items have no ids",
      JSON.stringify(
        picture({ ...busy(), turns: [{ id: 1, items: [{ kind: { type: "Words" } }] }] } as unknown as Chat, 1),
      ),
    ],
    [
      "a Picture whose words aren't text",
      JSON.stringify(
        picture(chat([turn(1, [{ ...words(2, ""), kind: { type: "Words", value: 5 } } as unknown as Item])]), 1),
      ),
    ],
    [
      "a Picture whose turn has no window",
      JSON.stringify(picture(chat([{ ...turn(1), window: null } as unknown as Turn]), 1)),
    ],
    ["Changes that aren't a list", JSON.stringify({ type: "Changes", value: { chat: 1000, changes: {} } })],
    ["Ended without its chat", JSON.stringify({ type: "Ended", value: {} })],
    ["a Done without its request", JSON.stringify({ type: "Done", value: { outcome: { type: "Done" } } })],
    ["a second Welcome", JSON.stringify(welcome)],
    ["NoCommonVersion after the Welcome", JSON.stringify({ type: "NoCommonVersion", value: { min: 1, max: 1 } })],
  ])("%s ends the link, and never throws", async (_, text) => {
    const { caf, link } = await connected();
    const pending = caf.stop(1000);
    let closes = 0;
    caf.onClose(() => void closes++);
    expect(() => link.raw(text)).not.toThrow();
    expect(link.closed).toBe(true);
    expect(closes).toBe(1);
    expect(await rejection(pending)).toStrictEqual({ type: "Disconnected" });
  });

  test("a change it can't read asks for a picture, and doesn't end the link", async () => {
    const { caf, link } = await connected();
    caf.watch(1000, () => {});
    link.deliver(picture(busy(), 10));
    link.take();
    link.deliver({ type: "Changes", value: { chat: 1000, changes: [{ no: 11, change: { type: "Appended" } }, null] } });
    expect(link.closed).toBe(false);
    expect(link.take()).toStrictEqual([
      { type: "Unwatch", value: { chat: 1000 } },
      { type: "Watch", value: { chat: 1000, since: null } },
    ]);
  });
});

describe("CafError", () => {
  test("names itself, and says why in its message", () => {
    const refused = new CafError({ type: "Refused", value: { type: "QueueFull" } });
    expect(refused.name).toBe("CafError");
    expect(refused.message).toBe("Refused: QueueFull");
    expect(String(refused)).toBe("CafError: Refused: QueueFull");
    expect(new CafError({ type: "Unknown", value: { outcome: "Postponed" } }).message).toBe("Unknown: Postponed");
    expect(new CafError({ type: "NoCommonVersion", value: { min: 2, max: 3 } }).message).toBe(
      "NoCommonVersion: CAF speaks versions 2 to 3",
    );
    expect(new CafError({ type: "Disconnected" }).message).toBe("Disconnected");
  });
});

describe("opening with a signal", () => {
  test("aborting before the Welcome rejects with the signal's reason, and closes the transport", async () => {
    const link = new FakeTransport();
    const abort = new AbortController();
    const opening = held(Connection.open(link, { signal: abort.signal }));
    abort.abort(new Error("gave up"));
    const result = await outcome(opening);
    expect(result !== "pending" && "error" in result && (result.error as Error).message).toBe("gave up");
    expect(link.closed).toBe(true);
  });

  test("a signal aborted already doesn't say Hello", async () => {
    const link = new FakeTransport();
    const opening = held(Connection.open(link, { signal: AbortSignal.abort() }));
    expect(await outcome(opening)).not.toBe("pending");
    expect(link.take()).toStrictEqual([]);
    expect(link.closed).toBe(true);
  });

  test.each([
    ["welcomed", welcome],
    ["refused", { type: "NoCommonVersion", value: { min: 2, max: 2 } }],
  ])("stops listening to the signal once %s", async (_, answer) => {
    const added: unknown[] = [];
    const removed: unknown[] = [];
    const signal = {
      aborted: false,
      reason: undefined,
      addEventListener: (type: string, listener: unknown) => void added.push([type, listener]),
      removeEventListener: (type: string, listener: unknown) => void removed.push([type, listener]),
    } as unknown as AbortSignal;
    const link = new FakeTransport();
    const opening = held(Connection.open(link, { signal }));
    link.deliver(answer as typeof welcome);
    await outcome(opening);
    expect(added).toHaveLength(1);
    expect(removed).toStrictEqual(added);
  });

  test("aborting after the Welcome does nothing", async () => {
    const link = new FakeTransport();
    const abort = new AbortController();
    const opening = Connection.open(link, { signal: abort.signal });
    link.deliver(welcome);
    const caf = await opening;
    abort.abort();
    expect(link.closed).toBe(false);
    expect(caf.run).toBe("run-1");
  });
});
