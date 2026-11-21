import { describe, expect, test } from "vitest";

import { apply, type Change, type Chat, type Mismatch, type Settings } from "../src/index.ts";
import { busy, chat, model, question, tool, toolItem, turn, words } from "./chats.ts";

/** Freezes a value all the way down, so any change to it throws. */
function frozen<T>(value: T): T {
  if (typeof value === "object" && value !== null) {
    for (const each of Object.values(value)) frozen(each);
    Object.freeze(value);
  }
  return value;
}

/** Applies a change that must fit, checking it left the old chat alone. */
function applied(before: Chat, change: Change): Chat {
  const copy = structuredClone(before);
  const result = apply(frozen(before), frozen(change));
  if (!("chat" in result)) throw new Error(`expected a chat, got ${JSON.stringify(result.mismatch)}`);
  expect(before).toStrictEqual(copy);
  expect(result.chat).not.toBe(before);
  return result.chat;
}

function mismatch(before: Chat, change: Change): Mismatch | "unknown-change" {
  const result = apply(frozen(before), frozen(change));
  if ("chat" in result) throw new Error("expected a mismatch");
  return result.mismatch;
}

describe("apply", () => {
  test("State sets the chat's state, sharing everything else", () => {
    const before = busy();
    const after = applied(before, { type: "State", value: { type: "Closed" } });
    expect(after.state).toStrictEqual({ type: "Closed" });
    expect(after.turns).toBe(before.turns);
    expect(after.questions).toBe(before.questions);
  });

  test("TurnAdded adds a turn at the end", () => {
    const before = busy();
    const added = turn(1100);
    const after = applied(before, { type: "TurnAdded", value: added });
    expect(after.turns.map((each) => each.id)).toStrictEqual([1001, 1003, 1100]);
    expect(after.turns[2]).toStrictEqual(added);
    expect(after.turns[0]).toBe(before.turns[0]);
    expect(after.turns[1]).toBe(before.turns[1]);
    expect(after.questions).toBe(before.questions);
  });

  test("TurnInserted adds a turn before another", () => {
    const before = busy();
    const after = applied(before, { type: "TurnInserted", value: { before: 1003, turn: turn(1100) } });
    expect(after.turns.map((each) => each.id)).toStrictEqual([1001, 1100, 1003]);
    expect(after.turns[0]).toBe(before.turns[0]);
    expect(after.turns[2]).toBe(before.turns[1]);
  });

  test("TurnInserted before the first turn", () => {
    const after = applied(busy(), { type: "TurnInserted", value: { before: 1001, turn: turn(1100) } });
    expect(after.turns.map((each) => each.id)).toStrictEqual([1100, 1001, 1003]);
  });

  test("TurnInserted before a turn the chat doesn't have", () => {
    expect(mismatch(busy(), { type: "TurnInserted", value: { before: 9, turn: turn(1100) } })).toStrictEqual({
      type: "NoTurn",
      value: 9,
    });
  });

  test("TurnLife sets a turn's life and its end", () => {
    const before = busy();
    const after = applied(before, {
      type: "TurnLife",
      value: { turn: 1003, life: { type: "Finished" }, end: { type: "At", value: 5 } },
    });
    const changed = after.turns[1]!;
    expect(changed.life).toStrictEqual({ type: "Finished" });
    expect(changed.window).toStrictEqual({ start: 1, end: { type: "At", value: 5 } });
    expect(changed.items).toBe(before.turns[1]!.items);
    expect(after.turns[0]).toBe(before.turns[0]);
  });

  test("TurnLife that doesn't end a turn keeps the end it's given", () => {
    const after = applied(busy(), {
      type: "TurnLife",
      value: { turn: 1001, life: { type: "Stopping" }, end: { type: "Open" } },
    });
    expect(after.turns[0]!.life).toStrictEqual({ type: "Stopping" });
    expect(after.turns[0]!.window.end).toStrictEqual({ type: "Open" });
  });

  test("TurnLife of a turn the chat doesn't have", () => {
    const change: Change = { type: "TurnLife", value: { turn: 9, life: { type: "Finished" }, end: { type: "Open" } } };
    expect(mismatch(busy(), change)).toStrictEqual({ type: "NoTurn", value: 9 });
  });

  test("ItemAdded adds an item to the end of a turn", () => {
    const before = busy();
    const after = applied(before, { type: "ItemAdded", value: { turn: 1001, item: words(1100, "more") } });
    expect(after.turns[0]!.items.map((each) => each.id)).toStrictEqual([1002, 1100]);
    expect(after.turns[0]!.items[0]).toBe(before.turns[0]!.items[0]);
    expect(after.turns[1]).toBe(before.turns[1]);
  });

  test("ItemAdded to a turn the chat doesn't have", () => {
    expect(mismatch(busy(), { type: "ItemAdded", value: { turn: 9, item: words(1100, "x") } })).toStrictEqual({
      type: "NoTurn",
      value: 9,
    });
  });

  test("Appended adds to words", () => {
    const before = busy();
    const after = applied(before, { type: "Appended", value: { item: 1004, text: ", world" } });
    const items = after.turns[1]!.items;
    expect(items[0]!.kind).toStrictEqual({ type: "Words", value: "Hello, world" });
    expect(items[0]!.window).toBe(before.turns[1]!.items[0]!.window);
    expect(items[1]).toBe(before.turns[1]!.items[1]);
    expect(items[2]).toBe(before.turns[1]!.items[2]);
    expect(after.turns[0]).toBe(before.turns[0]);
  });

  test("Appended adds to thinking", () => {
    const after = applied(busy(), { type: "Appended", value: { item: 1005, text: "m…" } });
    expect(after.turns[1]!.items[1]!.kind).toStrictEqual({ type: "Thinking", value: "Hmm…" });
  });

  test("Appended to an item in an older turn", () => {
    const after = applied(busy(), { type: "Appended", value: { item: 1002, text: "er" } });
    expect(after.turns[0]!.items[0]!.kind).toStrictEqual({ type: "Words", value: "older" });
  });

  test("Appended to an item the chat doesn't have, or one that isn't text", () => {
    expect(mismatch(busy(), { type: "Appended", value: { item: 9, text: "x" } })).toStrictEqual({
      type: "NoItem",
      value: 9,
    });
    expect(mismatch(busy(), { type: "Appended", value: { item: 1006, text: "x" } })).toStrictEqual({
      type: "NotText",
      value: 1006,
    });
  });

  test("ToolChanged replaces a tool call", () => {
    const before = busy();
    const now = { ...tool("ls -la"), status: { type: "Done" as const }, output: "a\nb" };
    const after = applied(before, { type: "ToolChanged", value: { item: 1006, tool: now } });
    const item = after.turns[1]!.items[2]!;
    expect(item.kind).toStrictEqual({ type: "Tool", value: now });
    expect(item.window).toBe(before.turns[1]!.items[2]!.window);
    expect(after.turns[1]!.items[0]).toBe(before.turns[1]!.items[0]);
  });

  test("ToolChanged of an item the chat doesn't have, or one that isn't a tool", () => {
    expect(mismatch(busy(), { type: "ToolChanged", value: { item: 9, tool: tool("x") } })).toStrictEqual({
      type: "NoItem",
      value: 9,
    });
    expect(mismatch(busy(), { type: "ToolChanged", value: { item: 1004, tool: tool("x") } })).toStrictEqual({
      type: "NotTool",
      value: 1004,
    });
  });

  test("ItemEnded sets an item's end", () => {
    const before = busy();
    const after = applied(before, { type: "ItemEnded", value: { item: 1006, end: { type: "At", value: 7 } } });
    const item = after.turns[1]!.items[2]!;
    expect(item.window).toStrictEqual({ start: 1, end: { type: "At", value: 7 } });
    expect(item.kind).toBe(before.turns[1]!.items[2]!.kind);
    expect(after.turns[1]!.items[1]).toBe(before.turns[1]!.items[1]);
  });

  test("ItemEnded of an item that has ended already sets it again", () => {
    const ended = chat([turn(1001, [{ ...words(1002, "x"), window: { start: 1, end: { type: "At", value: 2 } } }])]);
    const after = applied(ended, { type: "ItemEnded", value: { item: 1002, end: { type: "At", value: 3 } } });
    expect(after.turns[0]!.items[0]!.window.end).toStrictEqual({ type: "At", value: 3 });
  });

  test("ItemEnded of an item the chat doesn't have", () => {
    expect(mismatch(busy(), { type: "ItemEnded", value: { item: 9, end: { type: "Unknown" } } })).toStrictEqual({
      type: "NoItem",
      value: 9,
    });
  });

  test("Usage sets the chat's usage", () => {
    const before = busy();
    const usage = { used: 10, size: 200, cost: { amount: 0.5, currency: "USD" } };
    const after = applied(before, { type: "Usage", value: usage });
    expect(after.usage).toStrictEqual(usage);
    expect(after.turns).toBe(before.turns);
  });

  test("Settings replaces the chat's settings whole, sharing everything else", () => {
    const before: Chat = { ...busy(), settings: { offered: [model("default")], chosen: [] } };
    const settings: Settings = {
      offered: [model("sonnet")],
      chosen: [{ kind: { type: "Model" }, value: { type: "Choice", value: "haiku" } }],
    };
    const after = applied(before, { type: "Settings", value: settings });
    expect(after.settings).toStrictEqual(settings);
    expect(after.state).toBe(before.state);
    expect(after.turns).toBe(before.turns);
    expect(after.questions).toBe(before.questions);
  });

  test("QuestionAsked adds a question at the end", () => {
    const before = busy();
    const after = applied(before, { type: "QuestionAsked", value: question(1100) });
    expect(after.questions.map((each) => each.id)).toStrictEqual([1007, 1100]);
    expect(after.questions[0]).toBe(before.questions[0]);
    expect(after.turns).toBe(before.turns);
  });

  test("QuestionGone takes a question away", () => {
    const before = chat([], [question(1007), question(1008)]);
    const after = applied(before, { type: "QuestionGone", value: 1007 });
    expect(after.questions.map((each) => each.id)).toStrictEqual([1008]);
    expect(after.questions[0]).toBe(before.questions[1]);
  });

  test("QuestionGone takes away the one it names, not the first", () => {
    const before = chat([], [question(1007), question(1008), question(1009)]);
    const after = applied(before, { type: "QuestionGone", value: 1008 });
    expect(after.questions.map((each) => each.id)).toStrictEqual([1007, 1009]);
    expect(after.questions[0]).toBe(before.questions[0]);
    expect(after.questions[1]).toBe(before.questions[2]);
  });

  test("QuestionGone of a question the chat doesn't have", () => {
    expect(mismatch(busy(), { type: "QuestionGone", value: 9 })).toStrictEqual({ type: "NoQuestion", value: 9 });
  });

  test("a change of a type it doesn't know", () => {
    const later = { type: "TurnRenamed", value: { turn: 1001, name: "x" } } as unknown as Change;
    expect(mismatch(busy(), later)).toBe("unknown-change");
  });

  test("ids are found in the newest turn and item first, as the core looks", () => {
    // Ids are never reused, but if they were, the core's own lookup finds
    // the last one: so does apply.
    const twice = chat([turn(1001, [words(1002, "a")]), turn(1001, [words(1002, "b")])]);
    const after = applied(twice, { type: "Appended", value: { item: 1002, text: "!" } });
    expect(after.turns.map((each) => each.items[0]!.kind)).toStrictEqual([
      { type: "Words", value: "a" },
      { type: "Words", value: "b!" },
    ]);
    const life = applied(twice, {
      type: "TurnLife",
      value: { turn: 1001, life: { type: "Stopped" }, end: { type: "Open" } },
    });
    expect(life.turns.map((each) => each.life.type)).toStrictEqual(["Running", "Stopped"]);
  });

  test("a chat with nothing in it", () => {
    const empty = chat();
    expect(mismatch(empty, { type: "Appended", value: { item: 1, text: "x" } })).toStrictEqual({
      type: "NoItem",
      value: 1,
    });
    expect(mismatch(empty, { type: "ItemAdded", value: { turn: 1, item: toolItem(2, "x") } })).toStrictEqual({
      type: "NoTurn",
      value: 1,
    });
  });
});
