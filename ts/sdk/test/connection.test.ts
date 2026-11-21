import { afterEach, describe, expect, test, vi } from "vitest";

import {
  CafError,
  ChatCopy,
  Connection,
  type Change,
  type Chat,
  type DeepReadonly,
  type Item,
  type SavedChat,
  type Settings,
  type Turn,
  type WatchEvent,
} from "../src/index.ts";
import { busy, CHAT, chat, chatId, model, numbered, turn, words } from "./chats.ts";
import { agents, connected, FakeTransport, saved, welcome } from "./transport.ts";

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
  ({ type: "Changes", value: { chat: CHAT, changes: list } }) as const;
const wordsOf = (chat: DeepReadonly<Chat>) => chat.turns[1]!.items[0]!.kind;

/** A listener that keeps what it hears. */
function heard() {
  const events: WatchEvent[] = [];
  return { events, listener: (event: WatchEvent) => void events.push(event) };
}

/** A copy of chat 1000 at `latest`, as a connection to `run` made it. */
async function copyFrom(run: string, latest: number): Promise<ChatCopy> {
  const { caf, link } = await connected(run);
  const watch = caf.watch(CHAT, () => {});
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
    // Not yet: CAF says where its agents stand next.
    expect(await outcome(opening)).toBe("pending");
    link.deliver(agents);
    const caf = await opening;
    expect(caf.agents).toStrictEqual(agents.value);
    expect(caf.chats).toStrictEqual([{ id: CHAT, agent: "claude", cwd: "/home/me/project", label: null, shelf: null }]);
    expect(caf.run).toBe("run-1");
    expect(caf.may).toStrictEqual({ act: true, agents: true });
    expect(link.closed).toBe(false);
  });

  test.each([
    [{ act: false, agents: false }],
    [{ act: true, agents: false }],
    [{ act: false, agents: true }],
  ])("says what the host lets this viewer do: %j", async (may) => {
    const link = new FakeTransport();
    const opening = Connection.open(link);
    link.deliver({ ...welcome, value: { ...welcome.value, may } });
    link.deliver(agents);
    const caf = await opening;
    expect(caf.may).toStrictEqual(may);
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
    link.deliver({ type: "Motd", value: "hi" });
    link.deliver(agents);
    await expect(opening).resolves.toBeInstanceOf(Connection);
  });

  test("the link ending between the Welcome and the agents rejects with Disconnected", async () => {
    const link = new FakeTransport();
    const opening = Connection.open(link);
    link.deliver(welcome);
    link.drop();
    expect(await rejection(opening)).toStrictEqual({ type: "Disconnected" });
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
    ["a Welcome without what the viewer may do", JSON.stringify({ ...welcome, value: { ...welcome.value, may: undefined } })],
    [
      "a Welcome whose act isn't a boolean",
      JSON.stringify({ ...welcome, value: { ...welcome.value, may: { act: "yes", agents: true } } }),
    ],
    [
      "a Welcome whose agents isn't a boolean",
      JSON.stringify({ ...welcome, value: { ...welcome.value, may: { act: true, agents: "yes" } } }),
    ],
    [
      "a Welcome with a chat whose id is a number",
      JSON.stringify({ ...welcome, value: { ...welcome.value, chats: [{ id: 1000, agent: "claude", cwd: "/a" }] } }),
    ],
    [
      "a Welcome with a broken chat",
      JSON.stringify({ type: "Welcome", value: { version: 1, agents: [], chats: [{}] } }),
    ],
    ["a NoCommonVersion without its range", JSON.stringify({ type: "NoCommonVersion", value: {} })],
    ["another message of version 1", JSON.stringify({ type: "Chats", value: [] })],
    ["the agents before the Welcome", JSON.stringify(agents)],
  ])("%s first ends the link", async (_, text) => {
    const link = new FakeTransport();
    const opening = Connection.open(link);
    link.raw(text);
    expect(await rejection(opening)).toStrictEqual({ type: "Disconnected" });
    expect(link.closed).toBe(true);
    // And nothing after it changes that.
    link.deliver(welcome);
    link.deliver(agents);
  });

  test.each([
    ["text that isn't JSON", "{nope"],
    ["agents that aren't a list", JSON.stringify({ type: "Agents", value: {} })],
    ["an agent without its name", JSON.stringify({ type: "Agents", value: [{ ...agents.value[0], name: 1 }] })],
    [
      "an agent whose install state has no type",
      JSON.stringify({ type: "Agents", value: [{ ...agents.value[0], install: "Missing" }] }),
    ],
    [
      "an agent without its sign-in state",
      JSON.stringify({ type: "Agents", value: [{ name: "claude", install: { type: "Host" } }] }),
    ],
    [
      "an agent without whether it can be signed in to",
      JSON.stringify({ type: "Agents", value: [{ ...agents.value[0], can_sign_in: undefined }] }),
    ],
    [
      "an agent whose can_sign_in isn't a boolean",
      JSON.stringify({ type: "Agents", value: [{ ...agents.value[0], can_sign_in: "true" }] }),
    ],
    ["another message of version 1", JSON.stringify({ type: "Chats", value: [] })],
    ["the saved chats before the agents", JSON.stringify({ type: "Saved", value: [] })],
    ["a second Welcome", JSON.stringify(welcome)],
  ])("%s after the Welcome, in place of the agents, ends the link", async (_, text) => {
    const link = new FakeTransport();
    const opening = Connection.open(link);
    link.deliver(welcome);
    link.raw(text);
    expect(await rejection(opening)).toStrictEqual({ type: "Disconnected" });
    expect(link.closed).toBe(true);
    link.deliver(agents);
  });
});

describe("the agents", () => {
  test("follow Agents, until a listener stops", async () => {
    const { caf, link } = await connected();
    const lists: unknown[] = [];
    const stop = caf.onAgents((agents) => lists.push(agents));
    const next = [
      {
        name: "claude",
        install: { type: "Installing", value: { done: 1_000_000, size: 626_797_331 } },
        sign_in: { type: "SignedOut" },
        can_sign_in: false,
      },
    ];
    link.deliver({ type: "Agents", value: next });
    expect(caf.agents).toStrictEqual(next);
    expect(lists).toStrictEqual([next]);
    stop();
    link.deliver({ type: "Agents", value: [] });
    expect(caf.agents).toStrictEqual([]);
    expect(lists).toHaveLength(1);
  });

  test("the same listener twice is two subscriptions", async () => {
    const { caf, link } = await connected();
    let calls = 0;
    const listener = () => void calls++;
    const first = caf.onAgents(listener);
    caf.onAgents(listener);
    first();
    link.deliver(agents);
    expect(calls).toBe(1);
  });

  test("states of a later version come through as they are", async () => {
    const { caf, link } = await connected();
    const later = [
      {
        name: "claude",
        install: { type: "Updating", value: { from: "0.98.0" } },
        sign_in: { type: "Expired" },
        can_sign_in: true,
        extra: true,
      },
    ];
    link.deliver({ type: "Agents", value: later });
    expect(link.closed).toBe(false);
    expect(caf.agents).toStrictEqual(later);
  });

  test("what's known of each agent's settings comes through as it is", async () => {
    const { caf, link } = await connected();
    const statuses = [
      { ...agents.value[0], settings: { type: "Known", value: [model("default")] } },
      { ...agents.value[1], settings: { type: "Failed", value: "It wouldn't start." } },
    ];
    link.deliver({ type: "Agents", value: statuses });
    expect(link.closed).toBe(false);
    expect(caf.agents).toStrictEqual(statuses);
  });

  test("a listener that throws is reported, and the others still hear", async () => {
    const reported: unknown[] = [];
    vi.stubGlobal("reportError", (error: unknown) => reported.push(error));
    const { caf, link } = await connected();
    const boom = new Error("boom");
    let heard = 0;
    caf.onAgents(() => {
      throw boom;
    });
    caf.onAgents(() => void heard++);
    link.deliver(agents);
    expect(reported).toStrictEqual([boom]);
    expect(heard).toBe(1);
    expect(link.closed).toBe(false);
  });

  test("the link ending stops every listener", async () => {
    const { caf, link } = await connected();
    let heard = 0;
    caf.onAgents(() => void heard++);
    caf.disconnect();
    link.closed = false;
    link.deliver(agents);
    expect(heard).toBe(0);
  });
});

describe("the list of chats", () => {
  test("follows Chats, until a listener stops", async () => {
    const { caf, link } = await connected();
    const lists: unknown[] = [];
    const stop = caf.onChats((chats) => lists.push(chats));
    const next = [
      { id: CHAT, agent: "claude", cwd: "/a", label: "Fix the tests", shelf: "work" },
      { id: chatId(1100), agent: "fake", cwd: "/b", label: null, shelf: null },
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

describe("the saved chats", () => {
  test("are none until CAF says", async () => {
    const { caf } = await connected();
    expect(caf.saved).toStrictEqual([]);
  });

  test("come right after the agents, before open has resolved, or after", async () => {
    const link = new FakeTransport();
    const opening = Connection.open(link);
    link.deliver(welcome);
    link.deliver(agents);
    const list = [saved(1100), saved(1101, { type: "Loaded" })];
    link.deliver({ type: "Saved", value: list });
    const caf = await opening;
    expect(caf.saved).toStrictEqual(list);
    // Over a real link it comes once `open` has resolved: onSaved hears it.
    const lists: unknown[] = [];
    caf.onSaved((saved) => lists.push(saved));
    const next = [saved(1100, { type: "Restoring" }), saved(1101, { type: "Loaded" })];
    link.deliver({ type: "Saved", value: next });
    expect(caf.saved).toStrictEqual(next);
    expect(lists).toStrictEqual([next]);
    // A message of a later version after it is still ignored.
    link.deliver({ type: "Shelves", value: ["work"] });
    expect(link.closed).toBe(false);
    expect(caf.saved).toStrictEqual(next);
  });

  test("follow Saved, whole each time, until a listener stops", async () => {
    const { caf, link } = await connected();
    const lists: unknown[] = [];
    const stop = caf.onSaved((saved) => lists.push(saved));
    const failed = saved(1100, { type: "Failed", value: { type: "Newer", value: { version: 2 } } });
    link.deliver({ type: "Saved", value: [failed] });
    expect(caf.saved).toStrictEqual([failed]);
    stop();
    link.deliver({ type: "Saved", value: [] });
    expect(caf.saved).toStrictEqual([]);
    expect(lists).toStrictEqual([[failed]]);
  });

  test("states and reasons of a later version come through as they are", async () => {
    const { caf, link } = await connected();
    const later = [
      saved(1100, { type: "Moving" } as unknown as SavedChat["state"]),
      saved(1101, { type: "Failed", value: { type: "Encrypted" } } as unknown as SavedChat["state"]),
    ];
    link.deliver({ type: "Saved", value: later });
    expect(link.closed).toBe(false);
    expect(caf.saved).toStrictEqual(later);
  });

  test("the link ending stops every listener", async () => {
    const { caf, link } = await connected();
    let heard = 0;
    caf.onSaved(() => void heard++);
    caf.disconnect();
    link.closed = false;
    link.deliver({ type: "Saved", value: [] });
    expect(heard).toBe(0);
  });
});

describe("watching a chat", () => {
  test("asks for it, draws its picture, then its changes", async () => {
    const { caf, link } = await connected();
    const { events, listener } = heard();
    const watch = caf.watch(CHAT, listener);
    expect(link.take()).toStrictEqual([{ type: "Watch", value: { chat: CHAT, since: null } }]);
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

  test("follows its settings, whole each time, and asks again for settings it can't read", async () => {
    const { caf, link } = await connected();
    const { events, listener } = heard();
    caf.watch(CHAT, listener);
    link.deliver(picture(busy(), 10));
    link.take();
    // Sonnet chosen, and waiting: the agent still says Default.
    const settings: Settings = {
      offered: [model("default")],
      chosen: [{ kind: { type: "Model" }, value: { type: "Choice", value: "sonnet" } }],
      own: [{ type: "Model" }],
    };
    link.deliver(changes(numbered(11, { type: "Settings", value: settings })));
    const last = events.at(-1)!;
    expect(last.type === "Chat" && last.chat.settings).toStrictEqual(settings);
    link.deliver(changes(numbered(12, { type: "Settings", value: null } as never)));
    expect(link.closed).toBe(false);
    expect(link.take()).toStrictEqual([
      { type: "Unwatch", value: { chat: CHAT } },
      { type: "Watch", value: { chat: CHAT, since: null } },
    ]);
  });

  test("changes already had tell the listener nothing", async () => {
    const { caf, link } = await connected();
    const { events, listener } = heard();
    caf.watch(CHAT, listener);
    link.deliver(picture(busy(), 10));
    link.deliver(changes(numbered(9, append(" x")), numbered(10, append(" y"))));
    link.deliver(changes());
    expect(events).toHaveLength(1);
  });

  test("ignores other chats, and changes before the picture", async () => {
    const { caf, link } = await connected();
    const { events, listener } = heard();
    caf.watch(CHAT, listener);
    link.deliver(changes(numbered(11, append(" a"))));
    link.deliver(picture({ ...busy(), id: chatId(2000) }, 10));
    link.deliver({ type: "Changes", value: { chat: chatId(2000), changes: [numbered(11, append(" a"))] } });
    link.deliver({ type: "Ended", value: { chat: chatId(2000) } });
    expect(events).toStrictEqual([]);
    expect(link.take()).toStrictEqual([{ type: "Watch", value: { chat: CHAT, since: null } }]);
  });

  test.each<[string, Change]>([
    ["a gap", append(" c")],
    ["a change it doesn't know", { type: "TurnRenamed", value: {} } as unknown as Change],
    ["a change that doesn't fit", { type: "Appended", value: { item: 9, text: "x" } }],
  ])("%s asks for the chat whole again, once", async (_, bad) => {
    const { caf, link } = await connected();
    const { events, listener } = heard();
    const watch = caf.watch(CHAT, listener);
    link.deliver(picture(busy(), 10));
    link.take();

    const no = bad.type === "Appended" && bad.value.item === 1004 ? 13 : 12;
    link.deliver(changes(numbered(11, append(" a")), numbered(no, bad)));
    expect(link.take()).toStrictEqual([
      { type: "Unwatch", value: { chat: CHAT } },
      { type: "Watch", value: { chat: CHAT, since: null } },
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
    const watch = caf.watch(CHAT, listener);
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
    const watch = caf.watch(CHAT, listener);
    link.deliver(picture(busy(), 10));
    link.deliver({ type: "Ended", value: { chat: CHAT } });
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
    const watch = caf.watch(chatId(4242), listener);
    link.deliver({ type: "Ended", value: { chat: chatId(4242) } });
    expect(events).toStrictEqual([{ type: "Ended" }]);
    expect(watch.copy).toBeNull();
  });

  test("stop unwatches, and hears nothing more", async () => {
    const { caf, link } = await connected();
    const { events, listener } = heard();
    const watch = caf.watch(CHAT, listener);
    link.deliver(picture(busy(), 10));
    link.take();
    watch.stop();
    watch.stop();
    expect(link.take()).toStrictEqual([{ type: "Unwatch", value: { chat: CHAT } }]);
    link.deliver(changes(numbered(11, append(" a"))));
    link.deliver({ type: "Ended", value: { chat: CHAT } });
    expect(events).toHaveLength(1);
  });

  test("a listener that stops its watch mid-batch hears no more", async () => {
    const { caf, link } = await connected();
    const events: WatchEvent[] = [];
    const watch = caf.watch(CHAT, (event) => {
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
    const first = caf.watch(CHAT, one.listener);
    link.deliver(picture(busy(), 10));
    link.deliver(changes(numbered(11, append(" a"))));
    link.take();

    // CAF starts its watch again for the second, so both get a picture.
    const from = new ChatCopy(busy(), 3);
    const second = caf.watch(CHAT, two.listener, from);
    expect(link.take()).toStrictEqual([{ type: "Watch", value: { chat: CHAT, since: null } }]);
    link.deliver(picture(busy(), 11));
    link.deliver(changes(numbered(12, append(" b"))));
    expect(one.events).toHaveLength(4);
    expect(two.events).toHaveLength(2);
    expect(first.copy?.latest).toBe(12);
    expect(second.copy?.latest).toBe(12);

    // Both need the chat whole, and one asking gets both the picture.
    link.deliver(changes(numbered(14, append(" d"))));
    expect(link.take()).toStrictEqual([
      { type: "Unwatch", value: { chat: CHAT } },
      { type: "Watch", value: { chat: CHAT, since: null } },
    ]);
    link.deliver(picture(busy(), 20));
    expect(first.copy?.latest).toBe(20);
    expect(second.copy?.latest).toBe(20);

    first.stop();
    expect(link.take()).toStrictEqual([]);
    second.stop();
    expect(link.take()).toStrictEqual([{ type: "Unwatch", value: { chat: CHAT } }]);
  });

  test("resumes from a copy, leaving that copy as it was", async () => {
    const from = await copyFrom("run-1", 10);
    const { caf, link } = await connected();
    const { events, listener } = heard();
    const watch = caf.watch(CHAT, listener, from);
    expect(link.take()).toStrictEqual([{ type: "Watch", value: { chat: CHAT, since: 10 } }]);
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
    const watch = caf.watch(CHAT, listener, await copyFrom("run-1", 10));
    link.deliver(picture(busy(), 5000));
    expect(events).toHaveLength(1);
    expect(watch.copy?.latest).toBe(5000);
  });

  test("a copy of another chat isn't resumed from", async () => {
    const { caf, link } = await connected();
    const watch = caf.watch(CHAT, () => {}, new ChatCopy({ ...busy(), id: chatId(2000) }, 10));
    expect(link.take()).toStrictEqual([{ type: "Watch", value: { chat: CHAT, since: null } }]);
    expect(watch.copy).toBeNull();
  });

  test("a copy from another run of CAF isn't resumed from", async () => {
    // A restarted CAF numbers its chats and changes from the start again,
    // so chat 1000's change 10 there is another chat's.
    const from = await copyFrom("run-1", 10);
    const { caf, link } = await connected("run-2");
    const { events, listener } = heard();
    const watch = caf.watch(CHAT, listener, from);
    expect(link.take()).toStrictEqual([{ type: "Watch", value: { chat: CHAT, since: null } }]);
    expect(watch.copy).toBeNull();
    // Changes before the picture don't graft onto the old copy.
    link.deliver(changes(numbered(11, append(" a"))));
    expect(events).toStrictEqual([]);
    link.deliver(picture(busy(), 3));
    expect(watch.copy?.latest).toBe(3);
  });

  test("a copy the app made itself isn't resumed from", async () => {
    const { caf, link } = await connected();
    caf.watch(CHAT, () => {}, new ChatCopy(busy(), 10));
    expect(link.take()).toStrictEqual([{ type: "Watch", value: { chat: CHAT, since: null } }]);
  });

  test("a copy resumed from can be resumed from again, on a third connection", async () => {
    const from = await copyFrom("run-1", 10);
    const second = await connected();
    const watch = second.caf.watch(CHAT, () => {}, from);
    second.link.deliver(changes(numbered(11, append(" a"))));
    second.caf.disconnect();
    const third = await connected();
    third.caf.watch(CHAT, () => {}, watch.copy!);
    expect(third.link.take()).toStrictEqual([{ type: "Watch", value: { chat: CHAT, since: 11 } }]);
  });

  test("a chat that has Ended is watched afresh after", async () => {
    const from = await copyFrom("run-1", 10);
    const { caf, link } = await connected();
    caf.watch(CHAT, () => {});
    link.deliver({ type: "Ended", value: { chat: CHAT } });
    link.take();
    // No watch of it is left, so this one may resume, and is the last.
    const again = caf.watch(CHAT, () => {}, from);
    expect(link.take()).toStrictEqual([{ type: "Watch", value: { chat: CHAT, since: 10 } }]);
    again.stop();
    expect(link.take()).toStrictEqual([{ type: "Unwatch", value: { chat: CHAT } }]);
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
    const watch = caf.watch(CHAT, (event) => {
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
    const watch = caf.watch(CHAT, () => {
      if (++drawn === 2) watch.stop();
    });
    link.deliver(picture(busy(), 10));
    link.take();
    // 11 is drawn, and the listener stops; 13 then needs a picture.
    link.deliver(changes(numbered(11, append(" a")), numbered(13, append(" c"))));
    expect(link.take()).toStrictEqual([{ type: "Unwatch", value: { chat: CHAT } }]);
  });

  test("a picture that comes while the listener draws isn't asked for again", async () => {
    const { caf, link } = await connected();
    let drawing = 0;
    const watch = caf.watch(CHAT, () => {
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
    caf.watch(CHAT, () => {});
    link.deliver({ type: "Ended", value: { chat: CHAT } });
    caf.watch(CHAT, () => {}, from);
    link.take();
    link.deliver(changes(numbered(12, append(" b"))));
    expect(link.take()).toStrictEqual([
      { type: "Unwatch", value: { chat: CHAT } },
      { type: "Watch", value: { chat: CHAT, since: null } },
    ]);
  });

  test("a listener that throws is reported, and the link goes on", async () => {
    const reported: unknown[] = [];
    vi.stubGlobal("reportError", (error: unknown) => reported.push(error));
    const { caf, link } = await connected();
    const other = heard();
    caf.watch(CHAT, () => {
      throw new Error("the app's bug");
    });
    caf.watch(CHAT, other.listener);
    caf.onChats(() => {
      throw new Error("another");
    });
    link.deliver(picture(busy(), 10));
    link.deliver({ type: "Chats", value: [] });
    expect(reported.map((error) => (error as Error).message)).toStrictEqual(["the app's bug", "another"]);
    expect(other.events).toHaveLength(1);
    expect(link.closed).toBe(false);
    const sending = caf.stop(CHAT);
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
    caf.watch(CHAT, () => {
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
    link.deliver({ type: "Done", value: { request: 1, outcome: { type: "Opened", value: chatId(1100) } } });
    await expect(opening).resolves.toBe(chatId(1100));
  });

  test("restore asks for the saved chat on its shelf, and resolves with its id", async () => {
    const { caf, link } = await connected();
    const restoring = caf.restore(chatId(1100), "work");
    expect(link.take()).toStrictEqual([{ type: "Restore", value: { request: 1, chat: chatId(1100), shelf: "work" } }]);
    link.deliver({ type: "Done", value: { request: 1, outcome: { type: "Opened", value: chatId(1100) } } });
    await expect(restoring).resolves.toBe(chatId(1100));
  });

  test("a restore answered CannotRestore without its reason ends the link", async () => {
    const { caf, link } = await connected();
    const restoring = held(caf.restore(chatId(1100), "work"));
    link.deliver({ type: "Done", value: { request: 1, outcome: { type: "CannotRestore", value: "InUse" } } });
    expect(await rejection(restoring)).toStrictEqual({ type: "Disconnected" });
    expect(link.closed).toBe(true);
  });

  test("any other request answered CannotRestore ends the link", async () => {
    const { caf, link } = await connected();
    const stopping = held(caf.stop(CHAT));
    link.deliver({ type: "Done", value: { request: 1, outcome: { type: "CannotRestore", value: { type: "InUse" } } } });
    expect(await rejection(stopping)).toStrictEqual({ type: "Disconnected" });
    expect(link.closed).toBe(true);
  });

  test("restore rejects with why it couldn't be, or that it isn't allowed, and the link goes on", async () => {
    const { caf, link } = await connected();
    const inUse = held(caf.restore(chatId(1100), "work"));
    const newer = held(caf.restore(chatId(1101), "work"));
    const damaged = held(caf.restore(chatId(1102), "work"));
    const later = held(caf.restore(chatId(1103), "work"));
    const notAllowed = held(caf.restore(chatId(1104), "work"));
    const refused = held(caf.restore(chatId(1105), "../work"));
    const cannot = (request: number, why: unknown) =>
      link.deliver({ type: "Done", value: { request, outcome: { type: "CannotRestore", value: why } } });
    cannot(1, { type: "InUse" });
    cannot(2, { type: "Newer", value: { version: 3 } });
    cannot(3, { type: "Damaged", value: "line 4 isn't JSON" });
    // A reason of a later CAF's comes through, for the app to show by name.
    cannot(4, { type: "Encrypted" });
    link.deliver({ type: "Done", value: { request: 5, outcome: { type: "NotAllowed" } } });
    link.deliver({ type: "Done", value: { request: 6, outcome: { type: "Refused", value: { type: "BadShelf" } } } });
    expect(await rejection(inUse)).toStrictEqual({ type: "CannotRestore", value: { type: "InUse" } });
    expect(await rejection(newer)).toStrictEqual({ type: "CannotRestore", value: { type: "Newer", value: { version: 3 } } });
    expect(await rejection(damaged)).toStrictEqual({
      type: "CannotRestore",
      value: { type: "Damaged", value: "line 4 isn't JSON" },
    });
    expect(await rejection(later)).toStrictEqual({ type: "CannotRestore", value: { type: "Encrypted" } });
    expect(await rejection(notAllowed)).toStrictEqual({ type: "NotAllowed" });
    expect(await rejection(refused)).toStrictEqual({ type: "Refused", value: { type: "BadShelf" } });
    expect(link.closed).toBe(false);
  });

  test("send resolves with the turn", async () => {
    const { caf, link } = await connected();
    const sending = caf.send(CHAT, message);
    expect(link.take()).toStrictEqual([{ type: "Send", value: { request: 1, chat: CHAT, message } }]);
    link.deliver({ type: "Done", value: { request: 1, outcome: { type: "Sent", value: 1200 } } });
    await expect(sending).resolves.toBe(1200);
  });

  test("stop, answer and close resolve when done", async () => {
    const { caf, link } = await connected();
    const stopping = caf.stop(CHAT);
    const answering = caf.answer(1300, { type: "Permission", value: "allow" });
    const closing = caf.close(CHAT);
    expect(link.take()).toStrictEqual([
      { type: "Stop", value: { request: 1, chat: CHAT } },
      { type: "Answer", value: { request: 2, question: 1300, answer: { type: "Permission", value: "allow" } } },
      { type: "Close", value: { request: 3, chat: CHAT } },
    ]);
    // Out of order: each by its number.
    for (const request of [3, 1, 2]) link.deliver({ type: "Done", value: { request, outcome: { type: "Done" } } });
    await expect(Promise.all([stopping, answering, closing])).resolves.toStrictEqual([undefined, undefined, undefined]);
  });

  test("setSetting chooses a setting of the chat's agent, and resolves when done", async () => {
    const { caf, link } = await connected();
    const sonnet = { kind: { type: "Model" }, value: { type: "Choice", value: "sonnet" } } as const;
    const fast = { kind: { type: "Fast" }, value: { type: "Switch", value: true } } as const;
    const choosing = [caf.setSetting(CHAT, sonnet), caf.setSetting(CHAT, fast)];
    expect(link.take()).toStrictEqual([
      { type: "Set", value: { request: 1, chat: CHAT, choice: sonnet } },
      { type: "Set", value: { request: 2, chat: CHAT, choice: fast } },
    ]);
    for (const request of [2, 1]) link.deliver({ type: "Done", value: { request, outcome: { type: "Done" } } });
    await expect(Promise.all(choosing)).resolves.toStrictEqual([undefined, undefined]);
  });

  test("setSetting may be refused, saying the agent doesn't offer it, or not allowed, and the link goes on", async () => {
    const { caf, link } = await connected();
    const opus = { kind: { type: "Model" }, value: { type: "Choice", value: "opus" } } as const;
    const refused = held(caf.setSetting(CHAT, opus));
    const notAllowed = held(caf.setSetting(CHAT, opus));
    link.deliver({ type: "Done", value: { request: 1, outcome: { type: "Refused", value: { type: "NoSuchSetting" } } } });
    link.deliver({ type: "Done", value: { request: 2, outcome: { type: "NotAllowed" } } });
    expect(await rejection(refused)).toStrictEqual({ type: "Refused", value: { type: "NoSuchSetting" } });
    expect(await rejection(notAllowed)).toStrictEqual({ type: "NotAllowed" });
    expect(link.closed).toBe(false);
  });

  test("setSetting answered with an outcome that doesn't fit ends the link", async () => {
    const { caf, link } = await connected();
    const choosing = held(caf.setSetting(CHAT, { kind: { type: "Fast" }, value: { type: "Switch", value: false } }));
    link.deliver({ type: "Done", value: { request: 1, outcome: { type: "Sent", value: 1200 } } });
    expect(await rejection(choosing)).toStrictEqual({ type: "Disconnected" });
    expect(link.closed).toBe(true);
  });

  test("each agent action asks about its agent, and resolves when done", async () => {
    const { caf, link } = await connected();
    const acting = [
      caf.install("claude"),
      caf.stopInstall("claude"),
      caf.signIn("claude"),
      caf.paste("claude", "code#123"),
      caf.stopSignIn("fake"),
      caf.signOut("claude"),
      caf.learnSettings("claude"),
    ];
    expect(link.take()).toStrictEqual([
      { type: "Agent", value: { request: 1, agent: "claude", action: { type: "Install" } } },
      { type: "Agent", value: { request: 2, agent: "claude", action: { type: "StopInstall" } } },
      { type: "Agent", value: { request: 3, agent: "claude", action: { type: "SignIn" } } },
      { type: "Agent", value: { request: 4, agent: "claude", action: { type: "Paste", value: "code#123" } } },
      { type: "Agent", value: { request: 5, agent: "fake", action: { type: "StopSignIn" } } },
      { type: "Agent", value: { request: 6, agent: "claude", action: { type: "SignOut" } } },
      { type: "Agent", value: { request: 7, agent: "claude", action: { type: "LearnSettings" } } },
    ]);
    for (const request of [7, 6, 5, 4, 3, 2, 1]) {
      link.deliver({ type: "Done", value: { request, outcome: { type: "Done" } } });
    }
    await expect(Promise.all(acting)).resolves.toStrictEqual(acting.map(() => undefined));
  });

  test("an agent action may be refused, not allowed, or gone", async () => {
    const { caf, link } = await connected();
    const installing = held(caf.install("claude"));
    const signing = held(caf.signIn("claude"));
    const pasting = held(caf.paste("claude", "x"));
    const signingOut = held(caf.signOut("claude"));
    const learning = held(caf.learnSettings("claude"));
    link.deliver({ type: "Done", value: { request: 1, outcome: { type: "Refused", value: { type: "AlreadyInstalled" } } } });
    link.deliver({ type: "Done", value: { request: 2, outcome: { type: "NotAllowed" } } });
    link.deliver({ type: "Done", value: { request: 3, outcome: { type: "Gone" } } });
    link.deliver({ type: "Done", value: { request: 4, outcome: { type: "Retry" } } });
    link.deliver({ type: "Done", value: { request: 5, outcome: { type: "Refused", value: { type: "OwnTools" } } } });
    expect(await rejection(installing)).toStrictEqual({ type: "Refused", value: { type: "AlreadyInstalled" } });
    expect(await rejection(signing)).toStrictEqual({ type: "NotAllowed" });
    expect(await rejection(pasting)).toStrictEqual({ type: "Gone" });
    expect(await rejection(signingOut)).toStrictEqual({ type: "Unknown", value: { outcome: "Retry" } });
    expect(await rejection(learning)).toStrictEqual({ type: "Refused", value: { type: "OwnTools" } });
    expect(link.closed).toBe(false);
  });

  test("an agent action answered with an outcome that doesn't fit ends the link", async () => {
    const { caf, link } = await connected();
    const installing = held(caf.install("claude"));
    link.deliver({ type: "Done", value: { request: 1, outcome: { type: "Opened", value: chatId(1100) } } });
    expect(await rejection(installing)).toStrictEqual({ type: "Disconnected" });
    expect(link.closed).toBe(true);
  });

  test("an agent action after the link has ended rejects with Disconnected", async () => {
    const { caf, link } = await connected();
    caf.disconnect();
    expect(await rejection(caf.install("claude"))).toStrictEqual({ type: "Disconnected" });
    expect(link.take()).toStrictEqual([]);
  });

  test("Refused, NotAllowed and Gone reject with CafError", async () => {
    const { caf, link } = await connected();
    const refused = held(caf.send(CHAT, message));
    const later = held(caf.stop(CHAT));
    const notAllowed = held(caf.open({ agent: "claude", cwd: "/", session: { type: "New" } }));
    const gone = held(caf.close(CHAT));
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
    const stopping = caf.stop(CHAT);
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
    const stopping = held(caf.stop(CHAT));
    const sending = caf.send(CHAT, message);
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
    ["Opened with a chat that's a number", { type: "Opened", value: 1100 }],
    ["an Open answered CannotRestore", { type: "CannotRestore", value: { type: "InUse" } }],
    ["a Refused without its reason", { type: "Refused", value: "QueueFull" }],
  ])("%s ends the link", async (_, bad) => {
    const { caf, link } = await connected();
    const opening = held(caf.open({ agent: "claude", cwd: "/", session: { type: "New" } }));
    const other = held(caf.stop(CHAT));
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
      held(caf.send(CHAT, { parts: [] })),
      held(caf.stop(CHAT)),
      held(caf.close(CHAT)),
      held(caf.install("claude")),
      held(caf.paste("claude", "code")),
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
    const stopping = caf.stop(CHAT);
    link.take();
    caf.disconnect();
    expect(link.closed).toBe(true);
    expect(closes).toBe(1);
    expect(await rejection(stopping)).toStrictEqual({ type: "Disconnected" });
    // Nothing goes out after it, and nothing comes in.
    expect(await rejection(caf.send(CHAT, { parts: [] }))).toStrictEqual({ type: "Disconnected" });
    expect(() => caf.watch(CHAT, () => {})).toThrow(CafError);
    try {
      caf.watch(CHAT, () => {});
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
    const watch = caf.watch(CHAT, listener);
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
    expect(await rejection(caf.stop(CHAT))).toStrictEqual({ type: "Disconnected" });
    expect(link.closed).toBe(true);
  });
});

describe("what CAF sends", () => {
  test("a message of a type it doesn't know is ignored", async () => {
    const { caf, link } = await connected();
    const { events, listener } = heard();
    caf.watch(CHAT, listener);
    link.deliver({ type: "Typing", value: { chat: CHAT } });
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
    ["Chats whose chat's id is a number", JSON.stringify({ type: "Chats", value: [{ id: 1000, agent: "claude", cwd: "/a" }] })],
    ["a Picture of a chat whose id is a number", JSON.stringify(picture({ ...busy(), id: 1000 } as unknown as Chat, 1))],
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
    ["Changes that aren't a list", JSON.stringify({ type: "Changes", value: { chat: CHAT, changes: {} } })],
    ["Changes for a chat whose id is a number", JSON.stringify({ type: "Changes", value: { chat: 1000, changes: [] } })],
    ["Ended without its chat", JSON.stringify({ type: "Ended", value: {} })],
    ["Ended for a chat whose id is a number", JSON.stringify({ type: "Ended", value: { chat: 1000 } })],
    ["Saved that isn't a list", JSON.stringify({ type: "Saved", value: {} })],
    ["a saved chat without its header", JSON.stringify({ type: "Saved", value: [{ shelf: "work", state: { type: "Saved" } }] })],
    ["a saved chat whose id isn't text", JSON.stringify({ type: "Saved", value: [{ ...saved(1100), header: { ...saved(1100).header, chat: 1100 } }] })],
    ["a saved chat without its shelf", JSON.stringify({ type: "Saved", value: [{ ...saved(1100), shelf: null }] })],
    ["a saved chat whose state isn't tagged", JSON.stringify({ type: "Saved", value: [{ ...saved(1100), state: "Saved" }] })],
    ["a Done without its request", JSON.stringify({ type: "Done", value: { outcome: { type: "Done" } } })],
    ["a second Welcome", JSON.stringify(welcome)],
    ["Agents that aren't a list", JSON.stringify({ type: "Agents", value: null })],
    ["an agent without its states", JSON.stringify({ type: "Agents", value: [{ name: "claude" }] })],
    ["NoCommonVersion after the Welcome", JSON.stringify({ type: "NoCommonVersion", value: { min: 1, max: 1 } })],
  ])("%s ends the link, and never throws", async (_, text) => {
    const { caf, link } = await connected();
    const pending = caf.stop(CHAT);
    let closes = 0;
    caf.onClose(() => void closes++);
    expect(() => link.raw(text)).not.toThrow();
    expect(link.closed).toBe(true);
    expect(closes).toBe(1);
    expect(await rejection(pending)).toStrictEqual({ type: "Disconnected" });
  });

  test("a change it can't read asks for a picture, and doesn't end the link", async () => {
    const { caf, link } = await connected();
    caf.watch(CHAT, () => {});
    link.deliver(picture(busy(), 10));
    link.take();
    link.deliver({ type: "Changes", value: { chat: CHAT, changes: [{ no: 11, change: { type: "Appended" } }, null] } });
    expect(link.closed).toBe(false);
    expect(link.take()).toStrictEqual([
      { type: "Unwatch", value: { chat: CHAT } },
      { type: "Watch", value: { chat: CHAT, since: null } },
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
    expect(new CafError({ type: "CannotRestore", value: { type: "InUse" } }).message).toBe("CannotRestore: InUse");
    expect(new CafError({ type: "NoCommonVersion", value: { min: 2, max: 3 } }).message).toBe(
      "NoCommonVersion: CAF speaks versions 2 to 3",
    );
    expect(new CafError({ type: "Disconnected" }).message).toBe("Disconnected");
  });
});

describe("opening with a signal", () => {
  test.each([
    ["before the Welcome", []],
    ["between the Welcome and the agents", [welcome]],
  ])("aborting %s rejects with the signal's reason, and closes the transport", async (_, before) => {
    const link = new FakeTransport();
    const abort = new AbortController();
    const opening = held(Connection.open(link, { signal: abort.signal }));
    for (const message of before) link.deliver(message);
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
    ["welcomed", [welcome, agents]],
    ["refused", [{ type: "NoCommonVersion", value: { min: 2, max: 2 } }]],
  ])("stops listening to the signal once %s", async (_, answers) => {
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
    for (const answer of answers) link.deliver(answer);
    await outcome(opening);
    expect(added).toHaveLength(1);
    expect(removed).toStrictEqual(added);
  });

  test("aborting after the agents does nothing", async () => {
    const link = new FakeTransport();
    const abort = new AbortController();
    const opening = Connection.open(link, { signal: abort.signal });
    link.deliver(welcome);
    link.deliver(agents);
    const caf = await opening;
    abort.abort();
    expect(link.closed).toBe(false);
    expect(caf.run).toBe("run-1");
  });
});
